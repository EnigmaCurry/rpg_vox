//! Text-to-speech pipeline.
//!
//! The runner owns the ring-buffer producer and consumes [`Command`]s from
//! HTTP. Each `/say` or `/widgets` request carries a **voice profile** — a
//! list of one or more [`VoiceConfig`]s. Each config is a self-contained
//! recipe (speaker + language + instruct for the TTS backend, plus post-
//! processing knobs: pitch, detune, time, pan, gain, delay). A pluggable
//! [`Backend`] renders one config into mono f32 PCM; [`synthesize_profile`]
//! runs each config in the profile through the backend, applies per-config
//! DSP, and sums the results into a single stereo buffer at the pipewire
//! target rate. Profiles with N>1 configs produce a "hive-mind" — N voices
//! layered with independent pan / gain / delay — from one text prompt.
//!
//! The mixed stereo buffer is what feeds both the pipewire mic
//! (`Command::Say`) and the widget WAV writer (`Command::Synthesize`), so
//! saved clips preserve pan and layering by construction.

pub mod chunker;
pub mod comfyui;
pub mod piper;
pub mod qwen3;

use anyhow::Result;
use rtrb::Producer;
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info};

use crate::mixer::AtomicMixer;

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
    /// Synthesize a voice profile and push the mixed stereo buffer into the
    /// pipewire ring buffer (mic path).
    Say(SayRequest),
    /// Synthesize a voice profile into a stereo PCM buffer and return it to
    /// the caller. Nothing touches the mic. Used by the browser-only Speak
    /// widget.
    Synthesize(SynthesizeRequest),
    /// Push a pre-decoded stereo PCM buffer into the pipewire ring buffer
    /// and wait until playback finishes. Used to play cached widget clips
    /// through the mic (Scenes tab).
    PlayPcm(PlayPcmRequest),
}

/// One voice recipe within a profile. A profile with N of these fans out
/// into N concurrent synth calls that are mixed together per-config
/// pan/gain/delay in [`synthesize_profile`].
///
/// * `speaker`/`language`/`instruct` are TTS-backend inputs. Only [`qwen3`]
///   consumes them — other backends silently ignore.
/// * `pitch_semitones` + `detune_cents/100` are combined into a single
///   pitch shift and applied to the rendered PCM by [`apply_effects`].
/// * `time_ratio` is a duration multiplier applied after pitch shift.
/// * `gain_db` scales the mono buffer before the pan/delay mix.
/// * `pan` positions the voice in the stereo field via equal-power law
///   (`-1.0` = full L, `+1.0` = full R, `0.0` = center).
/// * `delay_ms` staggers the voice's start relative to the profile's mix.
#[derive(Debug, Clone)]
pub struct VoiceConfig {
    pub speaker: Option<String>,
    pub language: Option<String>,
    pub instruct: Option<String>,
    pub pitch_semitones: f32,
    pub time_ratio: f32,
    pub detune_cents: f32,
    pub pan: f32,
    pub gain_db: f32,
    pub delay_ms: f32,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            speaker: None,
            language: None,
            instruct: None,
            pitch_semitones: 0.0,
            time_ratio: 1.0,
            detune_cents: 0.0,
            pan: 0.0,
            gain_db: 0.0,
            delay_ms: 0.0,
        }
    }
}

impl VoiceConfig {
    /// Build the backend-facing [`VoiceOverride`] for this config. Detune is
    /// folded into `pitch_semitones` (100 cents = 1 semitone) so the DSP
    /// pass sees a single combined pitch shift.
    fn to_voice_override(&self) -> VoiceOverride {
        VoiceOverride {
            speaker: self.speaker.clone(),
            language: self.language.clone(),
            instruct: self.instruct.clone(),
            pitch_semitones: self.pitch_semitones + self.detune_cents / 100.0,
            time_ratio: self.time_ratio,
        }
    }
}

/// Per-request backend voice fields. Assembled from a [`VoiceConfig`] and
/// forwarded to the backend + DSP passes. Only [`qwen3`] consumes the
/// speaker/language/instruct fields.
#[derive(Debug, Clone, Default)]
pub struct VoiceOverride {
    pub speaker: Option<String>,
    pub language: Option<String>,
    pub instruct: Option<String>,
    pub pitch_semitones: f32,
    pub time_ratio: f32,
}

/// One utterance request: the text to speak, the voice profile to render
/// (as a list of layered configs), and a channel to report the result back
/// to the caller (usually the /say HTTP handler).
pub struct SayRequest {
    pub text: String,
    pub configs: Vec<VoiceConfig>,
    pub reply: oneshot::Sender<Result<usize, String>>,
}

/// Same shape as [`SayRequest`] but the reply carries the raw stereo PCM
/// instead of a frame count, since nothing plays it back automatically.
pub struct SynthesizeRequest {
    pub text: String,
    pub configs: Vec<VoiceConfig>,
    pub reply: oneshot::Sender<Result<SynthesizeOutcome, String>>,
}

/// Push already-decoded stereo PCM into the pipewire ring buffer. No
/// resampling — samples must already sit at the pipewire target rate
/// (widget WAVs are recorded that way, so this is always true for the
/// PlayPcm path). The runner waits for the ring to drain so the reply
/// lands when playback is truly over.
///
/// `seq` is the play-sequence claimed by the HTTP handler
/// ([`AtomicMixer::claim_play_seq`]). If a newer sequence exists by the
/// time the runner picks this command out of the mpsc, the handler skips
/// it — implementing latest-wins semantics so back-to-back clip clicks
/// don't stack up into a queued serial playback.
pub struct PlayPcmRequest {
    pub samples: Vec<[f32; 2]>,
    pub sample_rate: u32,
    pub seq: u64,
    pub reply: oneshot::Sender<Result<usize, String>>,
}

#[derive(Debug)]
pub struct SynthesizeOutcome {
    /// Stereo interleaved as pairs, already at [`Self::sample_rate`].
    pub samples: Vec<[f32; 2]>,
    /// The pipewire target rate.
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
    mixer: Arc<AtomicMixer>,
) -> Result<()> {
    while let Some(cmd) = rx.recv().await {
        match cmd {
            Command::Say(req) => handle_say(&mut backend, &cfg, &mut producer, req).await,
            Command::Synthesize(req) => handle_synthesize(&mut backend, &cfg, req).await,
            Command::PlayPcm(req) => handle_play_pcm(&cfg, &mixer, &mut producer, req).await,
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
        configs,
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
    info!(
        backend = backend.kind(),
        chars = text.len(),
        layers = configs.len(),
        "generating speech"
    );

    let result: Result<usize, String> = match synthesize_profile(backend, cfg, &configs, &text).await
    {
        Ok(stereo) => {
            let frames = stereo.len();
            match push_stereo_backpressured(producer, &stereo).await {
                pushed if pushed == frames => {
                    info!(frames = pushed, "utterance delivered to ring buffer");
                    Ok(pushed)
                }
                pushed => {
                    // Backpressured push never gives up on its own, so a short
                    // count here would only happen if the ring buffer path
                    // added an early-exit later. Report the actual count.
                    info!(frames = pushed, wanted = frames, "utterance push short");
                    Ok(pushed)
                }
            }
        }
        Err(err) => {
            error!(err = %err, "utterance failed");
            Err(err)
        }
    };
    let _ = reply.send(result);
}

async fn handle_synthesize(backend: &mut Backend, cfg: &Config, req: SynthesizeRequest) {
    let SynthesizeRequest {
        text,
        configs,
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
        layers = configs.len(),
        "synthesizing clip (no mic)"
    );
    let result = match synthesize_profile(backend, cfg, &configs, &text).await {
        Ok(samples) => {
            let sample_rate = cfg.target_sample_rate;
            info!(
                frames = samples.len(),
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
            error!(err = %err, "clip synthesis failed");
            Err(err)
        }
    };
    let _ = reply.send(result);
}

async fn handle_play_pcm(
    cfg: &Config,
    mixer: &Arc<AtomicMixer>,
    producer: &mut Producer<[f32; 2]>,
    req: PlayPcmRequest,
) {
    let PlayPcmRequest {
        samples,
        sample_rate,
        seq,
        reply,
    } = req;
    // Latest-wins: skip if a newer PlayPcm was claimed after us. Any
    // superseded request replies cleanly with 0 frames so the HTTP caller
    // learns it was dropped rather than hanging.
    if seq != mixer.latest_play_seq() {
        info!(seq, latest = mixer.latest_play_seq(), "cached PCM playback superseded, skipping");
        let _ = reply.send(Ok(0));
        return;
    }
    if samples.is_empty() {
        let _ = reply.send(Ok(0));
        return;
    }
    if sample_rate != cfg.target_sample_rate {
        // Widgets are always saved at the pipewire target rate (see the
        // /widgets encoder), so a mismatch here means something upstream
        // changed. Surface it instead of silently pitching the clip.
        let msg = format!(
            "PlayPcm sample_rate {} != target {}; refusing to play at wrong pitch",
            sample_rate, cfg.target_sample_rate
        );
        error!(err = %msg, "cached PCM play refused");
        let _ = reply.send(Err(msg));
        return;
    }
    info!(
        frames = samples.len(),
        sample_rate,
        "playing cached stereo PCM through mic"
    );

    // Snapshot the stop-generation NOW so a race between HTTP setup and
    // the push loop can't miss a stop that arrives while we're preparing.
    let start_gen = mixer.tts_stop_gen();
    let is_stopped = || mixer.tts_stop_gen() != start_gen;

    // Wait for the pw callback to actually drain the ring for any pending
    // stop before we start pushing this clip. Without this wait, a fresh
    // Play right after a Stop can push samples that then get drained by
    // the still-pending stop — playback starts mid-clip. Bounded so a
    // wedged pw thread doesn't stall the runner forever; the fallback
    // ships-the-clip-anyway matches the pre-race behavior.
    let deadline = tokio::time::Instant::now() + Duration::from_millis(250);
    while mixer.tts_stop_observed_gen() < start_gen {
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!(
                start_gen,
                observed = mixer.tts_stop_observed_gen(),
                "pw stop-drain sync timed out; playing anyway"
            );
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // ~100 ms chunks: fine enough that a user-triggered stop is felt as
    // "instant" (one chunk boundary + one pipewire cycle = well under
    // 150 ms), coarse enough that per-chunk atomic loads are noise.
    let chunk_frames = ((cfg.target_sample_rate as usize) / 10).max(512);
    let mut frames_pushed = 0usize;
    for chunk in samples.chunks(chunk_frames) {
        if is_stopped() {
            info!(frames = frames_pushed, "cached PCM playback stopped mid-push");
            break;
        }
        let pushed_now =
            push_stereo_backpressured_until(producer, chunk, &is_stopped).await;
        frames_pushed += pushed_now;
        if pushed_now < chunk.len() {
            // Push aborted mid-chunk because a stop was requested.
            break;
        }
    }

    wait_for_ring_or_stop(
        producer,
        cfg.ringbuf_frames,
        cfg.target_sample_rate,
        &is_stopped,
    )
    .await;
    if is_stopped() {
        info!(frames = frames_pushed, "cached PCM playback aborted");
    } else {
        info!(frames = frames_pushed, "cached PCM playback finished");
    }
    let _ = reply.send(Ok(frames_pushed));
}

/// Fan a voice profile out into N synth calls, apply each config's DSP, and
/// sum the results into a single stereo buffer at the pipewire target rate.
///
/// Currently sequential: [`Backend::synthesize`] takes `&mut self` and the
/// enum's variants are stateful, so N Qwen3 calls happen one-after-another.
/// For a hive-mind of 2–4 layers this is workable; if it becomes a
/// bottleneck the qwen3 backend's HTTP methods are pure (`&self`) and could
/// be lifted into a parallel path.
///
/// Mix math:
/// * Per-config linear gain (`10^(gain_db/20)`) is folded into the mono
///   buffer before layering.
/// * Delay converts to a sample offset at the target rate.
/// * Pan uses equal-power law: `l = m·cos((p+1)·π/4)`, `r = m·sin(…)`. `p=0`
///   yields `l=r=m·√½ ≈ 0.707m`, matching the mono-duplication path's
///   single-source loudness closely enough that a solo config sounds the
///   same as the pre-profile pipeline.
/// * The mix length is `max(delay_i + len_i)` so a delayed layer still fits.
///   Sums are clamped to ±1.0 at each add.
async fn synthesize_profile(
    backend: &mut Backend,
    cfg: &Config,
    configs: &[VoiceConfig],
    text: &str,
) -> Result<Vec<[f32; 2]>, String> {
    if configs.is_empty() {
        return Err("no voice configs in profile".into());
    }
    let target_rate = cfg.target_sample_rate;

    // (mono_samples, config_ref). Sample rate is always target_rate because
    // the capture_only Sink resamples on push.
    let mut layers: Vec<(Vec<f32>, &VoiceConfig)> = Vec::with_capacity(configs.len());
    for cc in configs {
        let voice = cc.to_voice_override();
        let mut sink = Sink::capture_only(target_rate);
        backend
            .synthesize(text, &voice, &mut sink)
            .await
            .map_err(|e| format!("{e:#}"))?;
        let (samples, _rate) = sink.take_capture();
        let processed = apply_effects(samples, target_rate, voice.pitch_semitones, voice.time_ratio)
            .map_err(|e| format!("{e:#}"))?;
        let gain_lin = 10f32.powf(cc.gain_db / 20.0);
        let with_gain: Vec<f32> = if (gain_lin - 1.0).abs() < f32::EPSILON {
            processed
        } else {
            processed.into_iter().map(|s| s * gain_lin).collect()
        };
        layers.push((with_gain, cc));
    }

    let total_len = layers
        .iter()
        .map(|(s, c)| {
            let delay_samples = delay_ms_to_samples(c.delay_ms, target_rate);
            delay_samples + s.len()
        })
        .max()
        .unwrap_or(0);
    let mut mix: Vec<[f32; 2]> = vec![[0.0, 0.0]; total_len];
    for (samples, cc) in &layers {
        let (l_gain, r_gain) = pan_gains(cc.pan);
        let delay_samples = delay_ms_to_samples(cc.delay_ms, target_rate);
        for (i, &s) in samples.iter().enumerate() {
            let idx = delay_samples + i;
            if idx >= mix.len() {
                break;
            }
            let l = (mix[idx][0] + s * l_gain).clamp(-1.0, 1.0);
            let r = (mix[idx][1] + s * r_gain).clamp(-1.0, 1.0);
            mix[idx] = [l, r];
        }
    }
    Ok(mix)
}

fn delay_ms_to_samples(delay_ms: f32, sample_rate: u32) -> usize {
    if delay_ms <= 0.0 || !delay_ms.is_finite() {
        return 0;
    }
    ((delay_ms / 1000.0) * sample_rate as f32) as usize
}

/// Equal-power pan. `pan` in `[-1, +1]` — clamped defensively so a bad input
/// can't produce negative gains.
fn pan_gains(pan: f32) -> (f32, f32) {
    let p = pan.clamp(-1.0, 1.0);
    let angle = (p + 1.0) * std::f32::consts::FRAC_PI_4;
    (angle.cos(), angle.sin())
}

/// Block until the pipewire consumer has drained (nearly) every sample from
/// the ring OR the caller signals stop via `is_stopped`. Sleep interval
/// scales with how full the ring currently is so we don't spin, but is
/// capped so we notice both the drain finishing and a late stop promptly.
///
/// Takes `&mut` even though `slots()` only needs `&self` because
/// `rtrb::Producer` is `!Sync` — a shared reference to it isn't `Send` across
/// `await`, whereas an exclusive reference to a `Send` type is.
async fn wait_for_ring_or_stop(
    producer: &mut Producer<[f32; 2]>,
    capacity: usize,
    sample_rate: u32,
    is_stopped: &(dyn Fn() -> bool + Sync),
) {
    let empty_threshold = capacity.saturating_sub(64);
    let rate = sample_rate.max(1) as usize;
    loop {
        let free = producer.slots();
        if free >= empty_threshold {
            return;
        }
        if is_stopped() {
            tokio::time::sleep(Duration::from_millis(25)).await;
            return;
        }
        let held = capacity.saturating_sub(free);
        let ms = ((held * 1000) / rate).clamp(10, 100) as u64;
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
}

/// Push stereo pairs into the ring, awaiting when full. Never drops audio.
async fn push_stereo_backpressured(
    producer: &mut Producer<[f32; 2]>,
    pairs: &[[f32; 2]],
) -> usize {
    let mut written = 0;
    while written < pairs.len() {
        while written < pairs.len() && producer.push(pairs[written]).is_ok() {
            written += 1;
        }
        if written < pairs.len() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    written
}

/// Stereo variant of [`push_stereo_backpressured`] that also stops pushing
/// when the caller signals stop. Returns the count of pairs that made it
/// into the ring (may be less than `pairs.len()` on an early stop).
async fn push_stereo_backpressured_until(
    producer: &mut Producer<[f32; 2]>,
    pairs: &[[f32; 2]],
    is_stopped: &(dyn Fn() -> bool + Sync),
) -> usize {
    let mut written = 0;
    while written < pairs.len() {
        if is_stopped() {
            return written;
        }
        while written < pairs.len() && producer.push(pairs[written]).is_ok() {
            written += 1;
        }
        if written < pairs.len() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    written
}

/// Apply pitch shift and time stretch to a mono f32 buffer.
///
/// Skips the DSP call entirely when both knobs sit at their identity values
/// so no-effect configs pay no CPU. Order is pitch first (to keep the
/// spectral envelope closer to the source), then time stretch. Extreme
/// values are clamped to avoid handing the algorithm something it can't
/// meaningfully process — a semitone range of ±24 covers character voices
/// (chipmunk to demon) and a 0.25×–4× time ratio covers the useful
/// slow/fast range.
fn apply_effects(
    samples: Vec<f32>,
    sample_rate: u32,
    pitch_semitones: f32,
    time_ratio: f32,
) -> Result<Vec<f32>> {
    let no_pitch = pitch_semitones.abs() <= f32::EPSILON;
    let no_time = (time_ratio - 1.0).abs() <= f32::EPSILON;
    if (no_pitch && no_time) || samples.is_empty() {
        return Ok(samples);
    }
    let semitones = pitch_semitones.clamp(-24.0, 24.0);
    let time_ratio = time_ratio.clamp(0.25, 4.0) as f64;

    let mut buf = samples;
    if !no_pitch {
        let factor = 2f64.powf(semitones as f64 / 12.0);
        let params = timestretch::StretchParams::new(1.0)
            .with_sample_rate(sample_rate)
            .with_channels(1);
        buf = timestretch::pitch_shift(&buf, &params, factor)
            .map_err(|e| anyhow::anyhow!("pitch_shift: {e}"))?;
    }
    if !no_time {
        let params = timestretch::StretchParams::new(time_ratio)
            .with_sample_rate(sample_rate)
            .with_channels(1);
        buf = timestretch::stretch(&buf, &params)
            .map_err(|e| anyhow::anyhow!("stretch: {e}"))?;
    }
    Ok(buf)
}

/// Capture-only PCM sink. Backends push mono `f32` audio chunks at their
/// native rate; the sink resamples to the pipewire target rate and buffers
/// the result in memory. [`synthesize_profile`] takes the captured buffer
/// per-config for the stereo mix.
///
/// The lifetime is vestigial (nothing borrows across the sink now that the
/// streaming path is gone) but kept so [`Backend::synthesize`]'s existing
/// signature `sink: &mut Sink<'_>` doesn't need to change for callers.
pub struct Sink<'a> {
    _marker: std::marker::PhantomData<&'a ()>,
    target_rate: u32,
    // (input_rate, resampler). Rebuilt when the incoming rate changes.
    resampler: Option<(u32, SincFixedIn<f32>)>,
    pub frames_pushed: usize,
    capture: Vec<f32>,
}

impl<'a> Sink<'a> {
    pub fn capture_only(target_rate: u32) -> Self {
        Self {
            _marker: std::marker::PhantomData,
            target_rate,
            resampler: None,
            frames_pushed: 0,
            capture: Vec::new(),
        }
    }

    /// Returns (samples, target_rate).
    pub fn take_capture(&mut self) -> (Vec<f32>, u32) {
        (std::mem::take(&mut self.capture), self.target_rate)
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

        self.frames_pushed += resampled.len();
        self.capture.extend_from_slice(&resampled);
        Ok(())
    }
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
