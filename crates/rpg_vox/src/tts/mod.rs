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
pub mod qwen3;

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

/// Serialized command stream feeding the single TTS backend. All commands
/// share the same backend instance so must run one-at-a-time; the single
/// mpsc + `while let` in [`run`] provides that.
pub enum Command {
    /// Synthesize and push into the pipewire ring buffer (mic path).
    Say(SayRequest),
    /// Synthesize into a PCM buffer and return it to the caller. Nothing
    /// touches the mic. Used by the browser-only Speak widget.
    Synthesize(SynthesizeRequest),
}

/// One utterance request: the text to speak plus a channel to report the
/// result back to the caller (usually the /say HTTP handler).
pub struct SayRequest {
    pub text: String,
    /// Per-request voice-style override. Currently only Qwen3 consumes it —
    /// other backends silently ignore. `None` means "use whatever the backend
    /// was configured with at startup".
    pub instruct: Option<String>,
    pub reply: oneshot::Sender<Result<usize, String>>,
}

/// Same shape as [`SayRequest`] but the reply carries the raw PCM instead of
/// a frame count, since nothing plays it back automatically.
pub struct SynthesizeRequest {
    pub text: String,
    pub instruct: Option<String>,
    pub reply: oneshot::Sender<Result<SynthesizeOutcome, String>>,
}

#[derive(Debug)]
pub struct SynthesizeOutcome {
    /// Mono f32 PCM.
    pub samples: Vec<f32>,
    /// The pipewire target rate — captured post-resample so downstream
    /// consumers get a single, consistent rate no matter which backend spoke.
    pub sample_rate: u32,
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
    Qwen3(qwen3::Backend),
}

impl Backend {
    /// Human-readable tag for logs.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Piper(_) => "piper",
            Self::Comfy(_) => "comfyui",
            Self::Qwen3(_) => "qwen3",
        }
    }

    /// Drive one utterance. The backend decides internal chunking (phrase
    /// splits for Piper, per-websocket-message for ComfyUI, single-request
    /// for remote Qwen3) and pushes each PCM chunk through the shared [`Sink`].
    ///
    /// `instruct` is a per-request voice-style override. Only [`qwen3`]
    /// consumes it; other backends ignore.
    pub async fn synthesize(
        &mut self,
        text: &str,
        instruct: Option<&str>,
        sink: &mut Sink<'_>,
    ) -> Result<()> {
        match self {
            Self::Piper(b) => b.synthesize(text, sink).await,
            Self::Comfy(b) => b.synthesize(text, sink).await,
            Self::Qwen3(b) => b.synthesize(text, instruct, sink).await,
        }
    }
}

pub async fn run(
    cfg: Config,
    mut backend: Backend,
    mut rx: mpsc::Receiver<Command>,
    mut producer: Producer<f32>,
) -> Result<()> {
    while let Some(cmd) = rx.recv().await {
        match cmd {
            Command::Say(req) => handle_say(&mut backend, &cfg, &mut producer, req).await,
            Command::Synthesize(req) => handle_synthesize(&mut backend, &cfg, req).await,
        }
    }
    Ok(())
}

async fn handle_say(
    backend: &mut Backend,
    cfg: &Config,
    producer: &mut Producer<f32>,
    req: SayRequest,
) {
    let SayRequest {
        text,
        instruct,
        reply,
    } = req;
    // Strip <think>…</think> here (not just in /chat) so any path that
    // reaches the mic — including a raw POST /say from a client that
    // forwarded LLM output verbatim — never speaks reasoning.
    let text = crate::chat::strip_thinking(&text).trim().to_string();
    if text.is_empty() {
        let _ = reply.send(Err("no speakable content after stripping <think>".into()));
        return;
    }
    info!(backend = backend.kind(), chars = text.len(), "generating speech");
    let mut sink = Sink::new(producer, cfg.target_sample_rate);
    let result = match backend
        .synthesize(&text, instruct.as_deref(), &mut sink)
        .await
    {
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

async fn handle_synthesize(backend: &mut Backend, cfg: &Config, req: SynthesizeRequest) {
    let SynthesizeRequest {
        text,
        instruct,
        reply,
    } = req;
    let text = crate::chat::strip_thinking(&text).trim().to_string();
    if text.is_empty() {
        let _ = reply.send(Err("no speakable content after stripping <think>".into()));
        return;
    }
    info!(
        backend = backend.kind(),
        chars = text.len(),
        "synthesizing clip (no mic)"
    );
    // Capture-only sink: pushes nowhere, just accumulates resampled PCM.
    let mut sink = Sink::capture_only(cfg.target_sample_rate);
    let result = match backend
        .synthesize(&text, instruct.as_deref(), &mut sink)
        .await
    {
        Ok(()) => {
            let (samples, sample_rate) = sink.take_capture();
            info!(
                samples = samples.len(),
                sample_rate,
                duration_ms = (samples.len() as u64 * 1000) / sample_rate.max(1) as u64,
                "clip synthesized"
            );
            Ok(SynthesizeOutcome {
                samples,
                sample_rate,
            })
        }
        Err(err) => {
            let msg = format!("{err:#}");
            error!(err = %msg, "clip synthesis failed");
            Err(msg)
        }
    };
    let _ = reply.send(result);
}

/// The resample-and-push side of the pipeline. Backends push PCM here without
/// knowing the pipewire target rate. Two modes:
///
/// * [`Sink::new`] — pushes into the pipewire producer for real-time
///   playback (the mic path).
/// * [`Sink::capture_only`] — has no producer; buffers all pushed PCM into
///   memory for the caller to take. Used by `/synthesize` when the caller
///   wants the audio bytes (e.g. to stream to a browser) instead of the mic.
pub struct Sink<'a> {
    producer: Option<&'a mut Producer<f32>>,
    target_rate: u32,
    // (input_rate, resampler). Rebuilt when the incoming rate changes.
    resampler: Option<(u32, SincFixedIn<f32>)>,
    pub frames_pushed: usize,
    capture: Option<Vec<f32>>,
}

impl<'a> Sink<'a> {
    pub fn new(producer: &'a mut Producer<f32>, target_rate: u32) -> Self {
        Self {
            producer: Some(producer),
            target_rate,
            resampler: None,
            frames_pushed: 0,
            capture: None,
        }
    }

    pub fn capture_only(target_rate: u32) -> Self {
        Self {
            producer: None,
            target_rate,
            resampler: None,
            frames_pushed: 0,
            capture: Some(Vec::new()),
        }
    }

    /// Returns (samples, target_rate). Empty Vec if capture was not enabled.
    pub fn take_capture(&mut self) -> (Vec<f32>, u32) {
        (self.capture.take().unwrap_or_default(), self.target_rate)
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

        if let Some(buf) = self.capture.as_mut() {
            buf.extend_from_slice(&resampled);
        }
        if let Some(producer) = self.producer.as_mut() {
            self.frames_pushed += push_samples_backpressured(*producer, &resampled).await;
        } else {
            // Capture-only sinks track "frames pushed" as capture length so
            // callers that read the field still get a sensible number.
            self.frames_pushed += resampled.len();
        }
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
