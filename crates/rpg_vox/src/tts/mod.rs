//! Text-to-speech pipeline.
//!
//! The runner owns the ring-buffer producer and consumes [`SayRequest`]s from
//! HTTP. A pluggable [`Backend`] (Piper in-process, or a ComfyUI websocket
//! session) turns each request into one or more [`AudioChunk`]s at its
//! native sample rate. The runner's [`Sink`] resamples to the pipewire target
//! rate and pushes into the ring, so backends don't have to think about
//! either.

pub mod chunker;
pub mod comfyui;
pub mod piper;

use anyhow::Result;
use rtrb::Producer;
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info};

pub use comfyui::warmup;

pub struct Config {
    pub target_sample_rate: u32,
}

/// One utterance request: the text to speak plus a channel to report the
/// result back to the caller (usually the /say HTTP handler).
pub struct SayRequest {
    pub text: String,
    pub reply: oneshot::Sender<Result<usize, String>>,
}

/// PCM chunk emitted by a TTS backend. `samples` is mono `f32` at
/// `sample_rate` Hz; the [`Sink`] resamples as needed.
pub struct AudioChunk {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

pub enum Backend {
    Piper(piper::Backend),
    Comfy(comfyui::Backend),
}

impl Backend {
    /// Human-readable tag for logs.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Piper(_) => "piper",
            Self::Comfy(_) => "comfyui",
        }
    }

    /// Drive one utterance. The backend decides internal chunking (phrase
    /// splits for Piper, per-websocket-message for ComfyUI) and pushes each
    /// PCM chunk through the shared [`Sink`].
    pub async fn synthesize(&mut self, text: &str, sink: &mut Sink<'_>) -> Result<()> {
        match self {
            Self::Piper(b) => b.synthesize(text, sink).await,
            Self::Comfy(b) => b.synthesize(text, sink).await,
        }
    }
}

pub async fn run(
    cfg: Config,
    mut backend: Backend,
    mut say_rx: mpsc::Receiver<SayRequest>,
    mut producer: Producer<f32>,
) -> Result<()> {
    while let Some(SayRequest { text, reply }) = say_rx.recv().await {
        // Strip <think>…</think> here (not just in /chat) so any path that
        // reaches the mic — including a raw POST /say from a client that
        // forwarded LLM output verbatim — never speaks reasoning.
        let text = crate::chat::strip_thinking(&text).trim().to_string();
        if text.is_empty() {
            let _ = reply.send(Err("no speakable content after stripping <think>".into()));
            continue;
        }
        info!(backend = backend.kind(), chars = text.len(), "generating speech");
        let mut sink = Sink::new(&mut producer, cfg.target_sample_rate);
        let result = match backend.synthesize(&text, &mut sink).await {
            Ok(()) => {
                info!(frames = sink.frames_pushed, "utterance delivered to ring buffer");
                Ok(sink.frames_pushed)
            }
            Err(err) => {
                let msg = format!("{err:#}");
                error!(err = %msg, "utterance failed");
                Err(msg)
            }
        };
        let _ = reply.send(result);
    }
    Ok(())
}

/// The resample-and-push side of the pipeline. Backends push PCM here without
/// knowing the pipewire target rate.
pub struct Sink<'a> {
    producer: &'a mut Producer<f32>,
    target_rate: u32,
    // (input_rate, resampler). Rebuilt when the incoming rate changes.
    resampler: Option<(u32, SincFixedIn<f32>)>,
    pub frames_pushed: usize,
}

impl<'a> Sink<'a> {
    pub fn new(producer: &'a mut Producer<f32>, target_rate: u32) -> Self {
        Self {
            producer,
            target_rate,
            resampler: None,
            frames_pushed: 0,
        }
    }

    pub async fn push(&mut self, chunk: AudioChunk) -> Result<()> {
        let AudioChunk {
            samples,
            sample_rate,
        } = chunk;

        let resampled = if sample_rate == self.target_rate {
            samples
        } else {
            let rs = match &mut self.resampler {
                Some((r, _)) if *r == sample_rate => {
                    self.resampler.as_mut().map(|(_, rs)| rs).unwrap()
                }
                _ => {
                    let rs = build_resampler(sample_rate, self.target_rate)?;
                    self.resampler = Some((sample_rate, rs));
                    &mut self.resampler.as_mut().unwrap().1
                }
            };
            resample(rs, &samples)?
        };

        self.frames_pushed += push_samples_backpressured(self.producer, &resampled).await;
        Ok(())
    }
}

/// Copy `samples` into the ring buffer, awaiting whenever it's full so the
/// pipewire consumer has a chance to drain. Never drops audio — a synthesized
/// utterance is much bigger than the ring, so backpressure is what keeps us
/// aligned with real-time playback instead of racing ahead and clipping.
async fn push_samples_backpressured(producer: &mut Producer<f32>, samples: &[f32]) -> usize {
    let mut written = 0;
    while written < samples.len() {
        while written < samples.len() && producer.push(samples[written]).is_ok() {
            written += 1;
        }
        if written < samples.len() {
            // ~half a frame at 48 kHz per 10 ms of sleep — fine granularity
            // for keeping the ring topped up without busy-looping.
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    written
}

fn build_resampler(input_rate: u32, output_rate: u32) -> Result<SincFixedIn<f32>> {
    let params = SincInterpolationParameters {
        sinc_len: 128,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Cubic,
        oversampling_factor: 128,
        window: WindowFunction::BlackmanHarris2,
    };
    let ratio = output_rate as f64 / input_rate as f64;
    Ok(SincFixedIn::<f32>::new(ratio, 2.0, params, 1024, 1)?)
}

fn resample(rs: &mut SincFixedIn<f32>, input: &[f32]) -> Result<Vec<f32>> {
    let chunk_size = rs.input_frames_next();
    let mut out = Vec::new();
    for chunk in input.chunks(chunk_size) {
        let mut padded;
        let slice: &[f32] = if chunk.len() == chunk_size {
            chunk
        } else {
            padded = vec![0.0f32; chunk_size];
            padded[..chunk.len()].copy_from_slice(chunk);
            &padded
        };
        let processed = rs.process(&[slice], None)?;
        out.extend_from_slice(&processed[0]);
    }
    Ok(out)
}
