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
    /// Total slot count of the pipewire ring buffer, used by [`PlayPcm`] to
    /// detect when the ring has fully drained so callers know audio has
    /// actually finished playing (not just been enqueued).
    pub ringbuf_frames: usize,
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
    /// Push a pre-decoded PCM buffer into the pipewire ring buffer and wait
    /// until playback finishes. Used to play cached widget clips through
    /// the mic (Scenes tab).
    PlayPcm(PlayPcmRequest),
}

/// Per-request voice overrides. Any field set to `Some` replaces the
/// backend's startup default for the duration of one synth call; `None`
/// falls back to the backend's configured value.
///
/// * `speaker`/`language`/`instruct` are TTS-backend inputs. Only [`qwen3`]
///   consumes them — other backends silently ignore.
/// * `pitch_semitones` and `time_ratio` are **post-processing** effects
///   applied to the rendered PCM by [`apply_effects`]. They work with any
///   backend. `pitch_semitones = 0.0` and `time_ratio = 1.0` are the
///   identity and short-circuit the pitch/stretch call.
#[derive(Debug, Clone)]
pub struct VoiceOverride {
    pub speaker: Option<String>,
    pub language: Option<String>,
    pub instruct: Option<String>,
    /// Pitch shift in semitones. Positive raises, negative lowers. Range is
    /// clamped in [`apply_effects`] so a wild value can't blow up the
    /// timestretch algorithm.
    pub pitch_semitones: f32,
    /// Duration multiplier. 1.0 = unchanged, 2.0 = twice as long (slower
    /// speech), 0.5 = half as long (faster). Independent of pitch.
    pub time_ratio: f32,
}

impl Default for VoiceOverride {
    fn default() -> Self {
        Self {
            speaker: None,
            language: None,
            instruct: None,
            pitch_semitones: 0.0,
            time_ratio: 1.0,
        }
    }
}

impl VoiceOverride {
    /// True when both effect knobs sit at their identity values — used to
    /// skip both the buffering and the DSP call on the common no-effect
    /// path.
    pub fn has_audio_effect(&self) -> bool {
        self.pitch_semitones.abs() > f32::EPSILON || (self.time_ratio - 1.0).abs() > f32::EPSILON
    }
}

/// One utterance request: the text to speak plus a channel to report the
/// result back to the caller (usually the /say HTTP handler).
pub struct SayRequest {
    pub text: String,
    pub voice: VoiceOverride,
    pub reply: oneshot::Sender<Result<usize, String>>,
}

/// Same shape as [`SayRequest`] but the reply carries the raw PCM instead of
/// a frame count, since nothing plays it back automatically.
pub struct SynthesizeRequest {
    pub text: String,
    pub voice: VoiceOverride,
    pub reply: oneshot::Sender<Result<SynthesizeOutcome, String>>,
}

/// Push already-decoded mono PCM into the pipewire ring buffer. The runner
/// resamples if `sample_rate` differs from the pipewire target, then waits
/// for the ring to drain so the reply lands when playback is truly over.
pub struct PlayPcmRequest {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub reply: oneshot::Sender<Result<usize, String>>,
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
    /// `voice` is a per-request voice override (speaker/language/instruct).
    /// Only [`qwen3`] consumes it; other backends ignore.
    pub async fn synthesize(
        &mut self,
        text: &str,
        voice: &VoiceOverride,
        sink: &mut Sink<'_>,
    ) -> Result<()> {
        match self {
            Self::Piper(b) => b.synthesize(text, sink).await,
            Self::Comfy(b) => b.synthesize(text, sink).await,
            Self::Qwen3(b) => b.synthesize(text, voice, sink).await,
        }
    }
}

pub async fn run(
    cfg: Config,
    mut backend: Backend,
    mut rx: mpsc::Receiver<Command>,
    mut producer: Producer<[f32; 2]>,
) -> Result<()> {
    while let Some(cmd) = rx.recv().await {
        match cmd {
            Command::Say(req) => handle_say(&mut backend, &cfg, &mut producer, req).await,
            Command::Synthesize(req) => handle_synthesize(&mut backend, &cfg, req).await,
            Command::PlayPcm(req) => handle_play_pcm(&cfg, &mut producer, req).await,
        }
    }
    Ok(())
}

async fn handle_say(
    backend: &mut Backend,
    cfg: &Config,
    producer: &mut Producer<[f32; 2]>,
    req: SayRequest,
) {
    let SayRequest {
        text,
        voice,
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

    // Two shapes for the mic path:
    //  * No effects — stream the resampled PCM straight into the ringbuf as
    //    the backend produces it (unchanged from before styles gained
    //    effects).
    //  * With effects — capture the full utterance first, run pitch/time
    //    through timestretch, then push the processed buffer through the
    //    producer. Adds latency (whole-utterance) but the algorithms need
    //    the full signal to do a decent job.
    let result: Result<usize, String> = if voice.has_audio_effect() {
        let mut sink = Sink::capture_only(cfg.target_sample_rate);
        match backend.synthesize(&text, &voice, &mut sink).await {
            Ok(()) => {
                let (samples, sample_rate) = sink.take_capture();
                match apply_effects(samples, sample_rate, &voice) {
                    Ok(processed) => {
                        // Wrap the final push in a Sink for uniformity with
                        // the no-effects path (both go through the source
                        // callback where the browser-monitor tap fires).
                        let mut push_sink = Sink::new(producer, cfg.target_sample_rate);
                        match push_sink
                            .push(AudioChunk {
                                samples: processed,
                                sample_rate: cfg.target_sample_rate,
                            })
                            .await
                        {
                            Ok(()) => {
                                info!(
                                    frames = push_sink.frames_pushed,
                                    "utterance (with effects) delivered to ring buffer"
                                );
                                Ok(push_sink.frames_pushed)
                            }
                            Err(err) => {
                                let msg = format!("{err:#}");
                                error!(err = %msg, "effect push failed");
                                Err(msg)
                            }
                        }
                    }
                    Err(err) => {
                        let msg = format!("{err:#}");
                        error!(err = %msg, "effect processing failed");
                        Err(msg)
                    }
                }
            }
            Err(err) => {
                let msg = format!("{err:#}");
                error!(err = %msg, "utterance failed");
                Err(msg)
            }
        }
    } else {
        let mut sink = Sink::new(producer, cfg.target_sample_rate);
        match backend.synthesize(&text, &voice, &mut sink).await {
            Ok(()) => {
                info!(frames = sink.frames_pushed, "utterance delivered to ring buffer");
                Ok(sink.frames_pushed)
            }
            Err(err) => {
                let msg = format!("{err:#}");
                error!(err = %msg, "utterance failed");
                Err(msg)
            }
        }
    };
    let _ = reply.send(result);
}

async fn handle_synthesize(backend: &mut Backend, cfg: &Config, req: SynthesizeRequest) {
    let SynthesizeRequest {
        text,
        voice,
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
        .synthesize(&text, &voice, &mut sink)
        .await
    {
        Ok(()) => {
            let (samples, sample_rate) = sink.take_capture();
            match apply_effects(samples, sample_rate, &voice) {
                Ok(samples) => {
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
                    error!(err = %msg, "effect processing failed");
                    Err(msg)
                }
            }
        }
        Err(err) => {
            let msg = format!("{err:#}");
            error!(err = %msg, "clip synthesis failed");
            Err(msg)
        }
    };
    let _ = reply.send(result);
}

async fn handle_play_pcm(cfg: &Config, producer: &mut Producer<[f32; 2]>, req: PlayPcmRequest) {
    let PlayPcmRequest {
        samples,
        sample_rate,
        reply,
    } = req;
    if samples.is_empty() {
        let _ = reply.send(Ok(0));
        return;
    }
    info!(
        samples = samples.len(),
        sample_rate,
        "playing cached PCM through mic"
    );

    let pushed = {
        let mut sink = Sink::new(producer, cfg.target_sample_rate);
        match sink
            .push(AudioChunk {
                samples,
                sample_rate,
            })
            .await
        {
            Ok(()) => sink.frames_pushed,
            Err(err) => {
                let msg = format!("{err:#}");
                error!(err = %msg, "cached PCM push failed");
                let _ = reply.send(Err(msg));
                return;
            }
        }
    };

    // Wait for the ring buffer to actually drain so the caller learns "done"
    // when audio has finished playing, not merely been enqueued. Poll rather
    // than compute-and-sleep because the ring may hold leftover audio from
    // an earlier /say command (mic path returns after push, not drain).
    wait_for_ring_empty(producer, cfg.ringbuf_frames, cfg.target_sample_rate).await;
    info!(frames = pushed, "cached PCM playback finished");
    let _ = reply.send(Ok(pushed));
}

/// Block until the pipewire consumer has drained (nearly) every sample from
/// the ring. Sleep interval scales with how full the ring currently is so we
/// don't spin, but is capped so we notice the drain finishing promptly.
///
/// Takes `&mut` even though `slots()` only needs `&self` because
/// `rtrb::Producer` is `!Sync` — a shared reference to it isn't `Send` across
/// `await`, whereas an exclusive reference to a `Send` type is.
async fn wait_for_ring_empty(producer: &mut Producer<[f32; 2]>, capacity: usize, sample_rate: u32) {
    // The ring is "empty" once the producer sees within a frame of the full
    // capacity as free slots. Slack absorbs small race between the consumer's
    // atomic write-back and this poll.
    let empty_threshold = capacity.saturating_sub(64);
    let rate = sample_rate.max(1) as usize;
    loop {
        let free = producer.slots();
        if free >= empty_threshold {
            return;
        }
        let held = capacity.saturating_sub(free);
        let ms = ((held * 1000) / rate).clamp(10, 250) as u64;
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
}

/// Apply per-style pitch shift and/or time stretch to a mono f32 buffer.
///
/// Skips the DSP call entirely when both knobs sit at their identity values
/// so no-effect styles pay no CPU. Order is pitch first (to keep the
/// spectral envelope closer to the source), then time stretch. Extreme
/// values are clamped to avoid handing the algorithm something it can't
/// meaningfully process — a semitone range of ±24 covers character voices
/// (chipmunk to demon) and a 0.25×–4× time ratio covers the useful
/// slow/fast range.
fn apply_effects(
    samples: Vec<f32>,
    sample_rate: u32,
    voice: &VoiceOverride,
) -> Result<Vec<f32>> {
    if !voice.has_audio_effect() || samples.is_empty() {
        return Ok(samples);
    }
    let semitones = voice.pitch_semitones.clamp(-24.0, 24.0);
    let time_ratio = voice.time_ratio.clamp(0.25, 4.0) as f64;

    // Mono in, mono out. `stretch_ratio` on StretchParams governs the
    // duration multiplier; for pitch_shift we keep it at 1.0 so pitch is
    // shifted without also stretching time. See docs.rs/timestretch.
    let mut buf = samples;
    if semitones.abs() > f32::EPSILON {
        let factor = 2f64.powf(semitones as f64 / 12.0);
        let params = timestretch::StretchParams::new(1.0)
            .with_sample_rate(sample_rate)
            .with_channels(1);
        buf = timestretch::pitch_shift(&buf, &params, factor)
            .map_err(|e| anyhow::anyhow!("pitch_shift: {e}"))?;
    }
    if (time_ratio - 1.0).abs() > f64::EPSILON {
        let params = timestretch::StretchParams::new(time_ratio)
            .with_sample_rate(sample_rate)
            .with_channels(1);
        buf = timestretch::stretch(&buf, &params)
            .map_err(|e| anyhow::anyhow!("stretch: {e}"))?;
    }
    Ok(buf)
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
    producer: Option<&'a mut Producer<[f32; 2]>>,
    target_rate: u32,
    // (input_rate, resampler). Rebuilt when the incoming rate changes.
    resampler: Option<(u32, SincFixedIn<f32>)>,
    pub frames_pushed: usize,
    capture: Option<Vec<f32>>,
}

impl<'a> Sink<'a> {
    pub fn new(producer: &'a mut Producer<[f32; 2]>, target_rate: u32) -> Self {
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
///
/// TTS is mono; the ring stores stereo frames. Each mono sample is duplicated
/// to `[m, m]` so the source callback's per-channel pan (constant-power)
/// produces the same acoustic result as the original mono-in-stereo-cloth
/// path did.
async fn push_samples_backpressured(producer: &mut Producer<[f32; 2]>, samples: &[f32]) -> usize {
    let mut written = 0;
    while written < samples.len() {
        while written < samples.len()
            && producer
                .push([samples[written], samples[written]])
                .is_ok()
        {
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
