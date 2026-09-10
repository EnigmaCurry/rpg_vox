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
pub mod clicks;
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

#[derive(Clone)]
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
    /// Distill a reference audio clip into a compact voice-prompt file via
    /// the Qwen3 clone deploy's `/save_prompt`. Only supported when the
    /// active backend is qwen3 AND its clone URL is configured; other
    /// backends reply with an error.
    SavePrompt(SavePromptRequest),
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
/// Which synthesis workflow to route this config through. Only [`qwen3`]
/// looks at this — piper/comfyui ignore mode and always synthesize plain
/// text through their single-model pipeline.
///
/// `Preset` uses the top-level `speaker` + `instruct` fields on the parent
/// [`VoiceConfig`]. `Clone` and `Design` carry mode-specific data inline
/// because they don't reuse those fields (and hoisting them to the top
/// level would leak clone-only data into preset paths).
#[derive(Debug, Clone)]
pub enum SynthMode {
    /// Preset speaker enum + optional `instruct` style prompt. Default.
    Preset,
    /// 3-second reference-audio clone. `voice_file_bytes` is the compact
    /// prompt file returned by `/save_prompt`; the HTTP layer pre-loads
    /// it from `data/voices/{id}.bin` so the backend doesn't need store
    /// access. `ref_txt` and `use_xvec` were baked into `voice_file_bytes`
    /// at save-prompt time, so we don't re-send them here.
    Clone {
        voice_file_bytes: Vec<u8>,
        /// Original filename to hint content-type on Gradio upload
        /// (usually `.bin` or `.pt` depending on Qwen3-TTS version).
        voice_file_name: String,
    },
    /// Voice generated from a free-text description on every request.
    /// No persistent state on the Qwen3 side — the description IS the
    /// voice identity.
    Design {
        description: String,
    },
    /// No new synth call — reuse the RAW mono synth output of another
    /// config in the same profile (0-based index). The copy still runs
    /// through its own pitch / time / FX / gain / pan / delay pipeline,
    /// so the caller can build a Mechanicum-style pitched-double layer by
    /// pointing this at the core voice and setting `pitch_semitones = -3`.
    ///
    /// The source's own FX chain is NOT inherited — copies see the raw
    /// synth output, not the post-FX signal — so copy layers can apply
    /// heavier processing without the source's filters cascading.
    /// [`synthesize_profile`] resolves dependencies in topological order
    /// and rejects cycles (self-copy, mutually-copying pairs, ...).
    Copy {
        from_index: usize,
    },
    /// User-uploaded audio clip looped to fit the profile's target
    /// duration (the longest synth sibling's length, or the sample's own
    /// duration when the profile has no synth voices). The looped mono
    /// buffer then runs through the same pitch / time / FX / gain / pan /
    /// delay pipeline as every other config, so a servo-motor recording
    /// becomes a full sound-design layer without extra plumbing.
    ///
    /// The HTTP layer preloads the bytes via `Store::get_sample_bytes` so
    /// the backend doesn't need store access at synth time.
    Sample {
        sample_bytes: Vec<u8>,
        sample_file_name: String,
    },
}

impl Default for SynthMode {
    fn default() -> Self {
        Self::Preset
    }
}

#[derive(Debug, Clone)]
pub struct VoiceConfig {
    /// Synthesis mode. Preset uses the fields below; Clone/Design carry
    /// mode-specific data inside the enum and IGNORE speaker/instruct.
    pub mode: SynthMode,
    /// Preset-mode speaker enum. Ignored by Clone/Design.
    pub speaker: Option<String>,
    /// Language enum — used by ALL modes (every Qwen3 endpoint takes
    /// `lang_disp`).
    pub language: Option<String>,
    /// Preset-mode style prompt. Ignored by Clone/Design.
    pub instruct: Option<String>,
    pub pitch_semitones: f32,
    pub time_ratio: f32,
    pub detune_cents: f32,
    pub pan: f32,
    pub gain_db: f32,
    pub delay_ms: f32,
    /// One-pole IIR bandpass corners applied to the post-pitch, post-time
    /// mono buffer. Either side at 0 (or at/above Nyquist) bypasses that
    /// leg. Together they let a config land the "destroyed vox-caster"
    /// bandpass with hpf≈250/lpf≈3200.
    pub hpf_hz: f32,
    pub lpf_hz: f32,
    /// tanh saturation drive in dB. Adds harmonics for tube / mechanical
    /// grit. Zero disables.
    pub drive_db: f32,
    /// Effective bit depth for the quantizer. Zero (or ≥16) disables;
    /// 6–10 gives a crunchy vox-caster edge on hard consonants.
    pub crush_bits: f32,
    /// Amplitude modulation / tremolo rate in Hz. Zero disables. Combined
    /// with a non-zero `am_depth`, multiplies the signal by
    /// `1 + depth·sin(2π·rate·t)` — ~47 Hz at ~0.3 depth is the Adeptus
    /// Mechanicum "servo motor buzz" the voice designer targets.
    pub am_rate_hz: f32,
    pub am_depth: f32,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            mode: SynthMode::default(),
            speaker: None,
            language: None,
            instruct: None,
            pitch_semitones: 0.0,
            time_ratio: 1.0,
            detune_cents: 0.0,
            pan: 0.0,
            gain_db: 0.0,
            delay_ms: 0.0,
            hpf_hz: 0.0,
            lpf_hz: 0.0,
            drive_db: 0.0,
            crush_bits: 0.0,
            am_rate_hz: 0.0,
            am_depth: 0.0,
        }
    }
}

impl VoiceConfig {
    /// Build the backend-facing [`VoiceOverride`] for this config. Detune is
    /// folded into `pitch_semitones` (100 cents = 1 semitone) so the DSP
    /// pass sees a single combined pitch shift.
    fn to_voice_override(&self) -> VoiceOverride {
        VoiceOverride {
            mode: self.mode.clone(),
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
/// mode/speaker/language/instruct fields.
#[derive(Debug, Clone, Default)]
pub struct VoiceOverride {
    pub mode: SynthMode,
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

/// One-shot request to have the backend upload a reference audio clip
/// to its clone Gradio and return the compact voice-prompt file. Bytes
/// come back through the `reply` oneshot; the caller is responsible for
/// persisting them (`Store::create_voice`). `filename` is preserved for
/// round-tripping on future load_prompt_and_gen uploads — Qwen3-TTS
/// versions differ on whether they return `.bin` or `.pt`.
pub struct SavePromptRequest {
    pub reference_wav: Vec<u8>,
    /// Filename to use on the multipart upload to Gradio. Extension
    /// affects Gradio's temp path but not correctness — `.wav` is
    /// always the right hint for audio references.
    pub reference_wav_name: String,
    pub ref_txt: String,
    pub use_xvec: bool,
    pub reply: oneshot::Sender<Result<SavePromptOutcome, String>>,
}

#[derive(Debug)]
pub struct SavePromptOutcome {
    /// Bytes of the voice-prompt file Gradio produced. Persist under
    /// `data/voices/{id}.{ext}` — the caller decides the extension by
    /// looking at `filename` below.
    pub voice_file_bytes: Vec<u8>,
    /// Filename Gradio returned (extension baked in). Preserve verbatim.
    pub filename: String,
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

/// Internal command for the play-side task: either a client-facing
/// PlayPcm request (from a cached widget) or a Say-synth product forwarded
/// from the backend task. `SayPush` bypasses the seq/stop semantics of
/// PlayPcm since Say is a fire-and-drain-in-full path.
enum PlayCmd {
    PlayPcm(PlayPcmRequest),
    SayPush {
        stereo: Vec<[f32; 2]>,
        reply: oneshot::Sender<Result<usize, String>>,
    },
    /// One "computer is thinking" click burst, generated by the clicks
    /// task. Reply fires after the push completes so the generator can
    /// pace itself against ring drain — no fixed sleep needed, and the
    /// pipeline naturally back-pressures if the ring is full. Any
    /// pending stop-gen bump (from a real playback command arriving) is
    /// honored inside the push so a click never lingers in the ring
    /// when real audio wants to preempt.
    ClickBurst {
        stereo: Vec<[f32; 2]>,
        reply: oneshot::Sender<()>,
    },
}

pub async fn run(
    cfg: Config,
    backend: Backend,
    mut rx: mpsc::Receiver<Command>,
    producer: Producer<[f32; 2]>,
    mixer: Arc<AtomicMixer>,
) -> Result<()> {
    // Three tasks so /script's "render block K+1 while playing block K"
    // pattern actually pipelines instead of serialising on any single
    // mpsc:
    //
    //   dispatcher-task: drains the PUBLIC `rx` and routes each command
    //                    to the right internal queue IMMEDIATELY. Fast
    //                    (single `send().await`) so PlayPcm never sits
    //                    behind an in-flight Synthesize.
    //   backend-task:    owns `Backend`, handles Synthesize / Say-synth.
    //                    Reads from `synth_rx`.
    //   play-task:       owns `producer`, handles PlayPcm / Say-push.
    //                    Reads from `play_rx`.
    //
    // Say bridges backend → play via `PlayCmd::SayPush`; its oneshot fires
    // from the play task after the ring push completes, matching the old
    // combined-runner semantics.
    let (synth_tx, synth_rx) = mpsc::channel::<Command>(32);
    let (play_tx, play_rx) = mpsc::channel::<PlayCmd>(32);

    let play_tx_disp = play_tx.clone();
    let dispatcher = tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                Command::PlayPcm(req) => {
                    if play_tx_disp.send(PlayCmd::PlayPcm(req)).await.is_err() {
                        break;
                    }
                }
                other => {
                    if synth_tx.send(other).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    let cfg_play = cfg.clone();
    let mixer_play = mixer.clone();
    let play_task = tokio::spawn(run_play(cfg_play, mixer_play, producer, play_rx));
    let cfg_bk = cfg.clone();
    let backend_task = tokio::spawn(run_backend(cfg_bk, backend, synth_rx, play_tx.clone()));
    // Fill-audio generator. Runs for the whole lifetime of the runner —
    // polls `mixer.clicks_active()` and sends burst commands into the
    // shared play channel whenever the wait-fill is enabled. When it's
    // disabled the task idles cheaply on a short sleep.
    let cfg_clicks = cfg.clone();
    let mixer_clicks = mixer.clone();
    let clicks_task = tokio::spawn(run_clicks(cfg_clicks, mixer_clicks, play_tx));

    dispatcher.await?;
    backend_task.await??;
    play_task.await??;
    clicks_task.await??;
    Ok(())
}

/// Wait-fill click generator loop. When `mixer.clicks_preset()` is
/// `Some(preset)`, hands ~120ms bursts of procedural clicks off to the
/// play task at a pace governed by the ring back-pressure (each burst
/// awaits its reply before the next is queued). When the mixer's
/// selection is `None` the loop idles on a short sleep. If the preset
/// changes mid-run the generator is rebuilt so the new voicing takes
/// effect at the next burst boundary. Never returns while the play
/// channel stays open.
async fn run_clicks(
    cfg: Config,
    mixer: Arc<AtomicMixer>,
    play_tx: mpsc::Sender<PlayCmd>,
) -> Result<()> {
    let sr = cfg.target_sample_rate;
    // 120 ms bursts: short enough that a real playback preemption via
    // request_tts_stop bounds the click tail to <200 ms in the worst
    // case (one burst already pushed to the ring gets flushed by the
    // pw callback within one process cycle).
    let burst_frames = ((sr as usize) * 120) / 1000;
    let idle_sleep = Duration::from_millis(50);
    let mut gen: Option<clicks::ClickGenerator> = None;
    loop {
        let selected = mixer.clicks_preset();
        let Some(preset) = selected else {
            // Cheap idle: 50 ms poll is imperceptible on the human side
            // and negligible CPU. Drop any previous generator so the
            // next "on" transition builds a fresh state — patterns don't
            // feel copy-pasted across waits.
            gen = None;
            tokio::time::sleep(idle_sleep).await;
            continue;
        };
        // First burst OR user swapped presets → new generator.
        let gref = match gen.as_ref() {
            Some(g) if g.preset() == preset => gen.as_mut().unwrap(),
            _ => {
                gen = Some(clicks::ClickGenerator::new(preset, sr));
                gen.as_mut().unwrap()
            }
        };
        let mut buf = vec![[0.0f32, 0.0f32]; burst_frames];
        gref.render_next(&mut buf);
        let (tx, rx) = oneshot::channel();
        if play_tx
            .send(PlayCmd::ClickBurst {
                stereo: buf,
                reply: tx,
            })
            .await
            .is_err()
        {
            // Play task gone → runner shutting down.
            return Ok(());
        }
        // Await the push completing. If the reply is dropped (play task
        // gone) we bail out same as above. Ring back-pressure inside the
        // push handler is what actually paces us.
        if rx.await.is_err() {
            return Ok(());
        }
    }
}

async fn run_backend(
    cfg: Config,
    mut backend: Backend,
    mut rx: mpsc::Receiver<Command>,
    play_tx: mpsc::Sender<PlayCmd>,
) -> Result<()> {
    while let Some(cmd) = rx.recv().await {
        match cmd {
            Command::Say(req) => handle_say(&mut backend, &cfg, &play_tx, req).await,
            Command::Synthesize(req) => handle_synthesize(&mut backend, &cfg, req).await,
            Command::PlayPcm(req) => {
                // Route PlayPcm to the dedicated play task so it doesn't
                // block the backend from processing the next Synthesize.
                if play_tx.send(PlayCmd::PlayPcm(req)).await.is_err() {
                    tracing::warn!("play task gone; dropping PlayPcm");
                }
            }
            Command::SavePrompt(req) => handle_save_prompt(&mut backend, req).await,
        }
    }
    Ok(())
}

/// Dispatch SavePrompt requests to the active backend. Only qwen3 knows
/// how — every other backend replies with a clear "not supported" so the
/// HTTP handler can surface a 400 rather than time out.
async fn handle_save_prompt(backend: &mut Backend, req: SavePromptRequest) {
    let SavePromptRequest {
        reference_wav,
        reference_wav_name,
        ref_txt,
        use_xvec,
        reply,
    } = req;
    let result = match backend {
        Backend::Qwen3(b) => b
            .save_prompt(reference_wav, reference_wav_name, ref_txt, use_xvec)
            .await
            .map_err(|e| format!("{e:#}")),
        Backend::Piper(_) | Backend::Comfy(_) => Err(format!(
            "voice cloning requires the qwen3 backend (currently: {})",
            backend.kind()
        )),
    };
    let _ = reply.send(result);
}

async fn run_play(
    cfg: Config,
    mixer: Arc<AtomicMixer>,
    mut producer: Producer<[f32; 2]>,
    mut rx: mpsc::Receiver<PlayCmd>,
) -> Result<()> {
    while let Some(cmd) = rx.recv().await {
        match cmd {
            PlayCmd::PlayPcm(req) => {
                handle_play_pcm(&cfg, &mixer, &mut producer, req).await;
            }
            PlayCmd::SayPush { stereo, reply } => {
                let frames = stereo.len();
                let pushed = push_stereo_backpressured(&mut producer, &stereo).await;
                info!(pushed, wanted = frames, "Say push delivered to ring");
                let _ = reply.send(Ok(pushed));
            }
            PlayCmd::ClickBurst { stereo, reply } => {
                handle_click_burst(&mixer, &mut producer, stereo).await;
                let _ = reply.send(());
            }
        }
    }
    Ok(())
}

/// Push a click burst into the ring, honoring both the mixer's preset
/// selection AND `tts_stop_gen`. Either being flipped mid-push aborts
/// immediately so a real playback command sitting behind us in the
/// mpsc doesn't wait for the whole burst before it can preempt.
async fn handle_click_burst(
    mixer: &Arc<AtomicMixer>,
    producer: &mut Producer<[f32; 2]>,
    stereo: Vec<[f32; 2]>,
) {
    // Snapshot the current stop-gen; if it moves while we're pushing the
    // burst, someone (typically a Say / PlayPcm handler) requested a stop
    // and we should stop feeding stale filler into the ring.
    let start_gen = mixer.tts_stop_gen();
    let is_stopped = || {
        mixer.tts_stop_gen() != start_gen || mixer.clicks_preset().is_none()
    };
    let mut written = 0usize;
    while written < stereo.len() {
        if is_stopped() {
            return;
        }
        while written < stereo.len() && producer.push(stereo[written]).is_ok() {
            written += 1;
        }
        if written < stereo.len() {
            // Ring is full — wait a short beat for the pw callback to
            // consume, then re-check the stop conditions. 10 ms matches
            // the granularity used elsewhere in this file.
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

async fn handle_say(
    backend: &mut Backend,
    cfg: &Config,
    play_tx: &mpsc::Sender<PlayCmd>,
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

    match synthesize_profile(backend, cfg, &configs, &text).await {
        Ok(stereo) => {
            // Hand off to play task. The reply oneshot fires from THERE
            // after the ring push completes, so the /say HTTP handler
            // still gets a single Result and blocks until the audio has
            // been queued — matching the old combined-runner behavior.
            if play_tx.send(PlayCmd::SayPush { stereo, reply }).await.is_err() {
                tracing::warn!("play task gone; Say bridge dropped");
            }
        }
        Err(err) => {
            error!(err = %err, "utterance failed");
            let _ = reply.send(Err(err));
        }
    };
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

    // Wait for the pw callback to actually drain the ring for any pending
    // stop before we start pushing this clip. Without this wait, a fresh
    // Play right after a Stop can push samples that then get drained by
    // the still-pending stop — playback starts mid-clip. Bounded so a
    // wedged pw thread doesn't stall the runner forever; the fallback
    // ships-the-clip-anyway matches the pre-race behavior.
    let sync_target = mixer.tts_stop_gen();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(250);
    while mixer.tts_stop_observed_gen() < sync_target {
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!(
                sync_target,
                observed = mixer.tts_stop_observed_gen(),
                "pw stop-drain sync timed out; playing anyway"
            );
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // Snapshot the stop-generation AFTER the sync. Bumps that arrived
    // during the sync are already accounted for by the drain — treating
    // them as "abort this push" would incorrectly cut off the tail of a
    // clip when a racing POST arrives around the same time as this play
    // (the /clicks/stop that fires from the client the moment activeClip
    // becomes a real speech clip is the canonical case). Only bumps that
    // arrive from HERE on should abort the push mid-clip.
    let start_gen = mixer.tts_stop_gen();
    let is_stopped = || mixer.tts_stop_gen() != start_gen;

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

    // Up-front validation of Copy references. Cheap and turns a
    // mis-configured profile into a clear error at request time instead
    // of a mid-synth panic.
    for (i, cc) in configs.iter().enumerate() {
        if let SynthMode::Copy { from_index } = &cc.mode {
            if *from_index >= configs.len() {
                return Err(format!(
                    "voice {} copies from voice {} but only {} voices exist in this profile",
                    i + 1,
                    *from_index + 1,
                    configs.len(),
                ));
            }
            if *from_index == i {
                return Err(format!("voice {} cannot copy from itself", i + 1));
            }
        }
    }

    // Phase 1 collects the RAW mono buffer per config in three sub-passes
    // so the sample-loop path can size itself against the synth voices'
    // natural durations:
    //
    //   1a. Preset / Clone / Design — run the backend, capture the raw
    //       synth output.
    //   1b. Sample — decode + resample + loop to `max(synth_lens)` so
    //       an atmospheric drone rides for the whole clip. When the
    //       profile has no synth voices at all, we fall back to each
    //       sample's natural length so an all-sample profile still plays.
    //   1c. Copy — in topological dep order, clone the source's raw buffer.
    //       Cycles + missing sources become clear errors.
    let mut raws: Vec<Option<Vec<f32>>> = vec![None; configs.len()];

    for i in 0..configs.len() {
        match &configs[i].mode {
            SynthMode::Preset | SynthMode::Clone { .. } | SynthMode::Design { .. } => {
                let voice = configs[i].to_voice_override();
                let mut sink = Sink::capture_only(target_rate);
                backend
                    .synthesize(text, &voice, &mut sink)
                    .await
                    .map_err(|e| format!("{e:#}"))?;
                let (samples, _rate) = sink.take_capture();
                raws[i] = Some(samples);
            }
            _ => {}
        }
    }

    let max_synth_len = raws
        .iter()
        .filter_map(|r| r.as_ref().map(|s| s.len()))
        .max()
        .unwrap_or(0);

    for i in 0..configs.len() {
        if let SynthMode::Sample { sample_bytes, sample_file_name } = &configs[i].mode {
            let decoded = decode_and_resample_sample(sample_bytes, target_rate)
                .map_err(|e| format!("voice {} sample \"{}\": {e}", i + 1, sample_file_name))?;
            let target_len = if max_synth_len > 0 { max_synth_len } else { decoded.len() };
            raws[i] = Some(loop_to_length(decoded, target_len));
        }
    }

    // Copies-in-dep-order. A pass that resolves at least one copy is
    // progress; a pass that resolves none while copies remain is a
    // cycle (mutual copies, N-way loop, or a copy pointing at another
    // copy whose source is also unresolved).
    let mut remaining_copies: usize = configs
        .iter()
        .filter(|cc| matches!(cc.mode, SynthMode::Copy { .. }))
        .count();
    while remaining_copies > 0 {
        let mut progressed = false;
        for i in 0..configs.len() {
            if raws[i].is_some() {
                continue;
            }
            let SynthMode::Copy { from_index } = &configs[i].mode else {
                continue;
            };
            if let Some(src) = raws[*from_index].as_ref() {
                raws[i] = Some(src.clone());
                remaining_copies -= 1;
                progressed = true;
            }
        }
        if !progressed {
            return Err(
                "copy cycle detected in voice profile (voices copying from each other)".into(),
            );
        }
    }

    // Phase 2: apply each config's own pitch/time/FX/gain to its raw
    // mono buffer, then hand off to the stereo mix. Copy layers run the
    // same pipeline as synth layers — the only thing that differed was
    // where their raw samples came from.
    let mut layers: Vec<(Vec<f32>, &VoiceConfig)> = Vec::with_capacity(configs.len());
    for (i, cc) in configs.iter().enumerate() {
        let voice = cc.to_voice_override();
        let raw = raws[i].take().unwrap_or_default();
        let mut processed = apply_effects(
            raw,
            target_rate,
            voice.pitch_semitones,
            voice.time_ratio,
        )
        .map_err(|e| format!("{e:#}"))?;
        apply_fx_chain(
            &mut processed,
            target_rate,
            &FxParams {
                hpf_hz: cc.hpf_hz,
                lpf_hz: cc.lpf_hz,
                drive_db: cc.drive_db,
                crush_bits: cc.crush_bits,
                am_rate_hz: cc.am_rate_hz,
                am_depth: cc.am_depth,
            },
        );
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
/// Skips the DSP call entirely when every knob sits at its identity value
/// so no-effect configs pay no CPU. Order is:
///   1. pitch shift (keeps the spectral envelope close to source)
///   2. time stretch (applied to the pitched buffer)
///
/// The old inline "doubler" pass has been replaced by [`SynthMode::Copy`]
/// configs — a copy layer's own pitch/time/FX/gain now stand in for what
/// the doubler used to do, giving each doubled voice a full effect chain
/// instead of a single semitone knob.
///
/// Extreme values are clamped so the algorithm never sees something it
/// can't meaningfully process: ±24 semitones covers chipmunk-to-demon and
/// 0.25×–4× time ratio covers the useful slow/fast range.
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

/// Grouped parameters for [`apply_fx_chain`]. Kept as a struct so the
/// caller doesn't hit clippy's `too_many_arguments` lint and so any
/// future FX knob is a one-field addition instead of another signature
/// change through the call chain.
struct FxParams {
    hpf_hz: f32,
    lpf_hz: f32,
    drive_db: f32,
    crush_bits: f32,
    am_rate_hz: f32,
    am_depth: f32,
}

/// Apply the "FX" chain (bandpass → saturation → bit-crush → AM) in-place
/// on a mono buffer. Each stage is a no-op when its knob sits at the
/// identity value so an all-default config still pays only the branch cost.
///
/// Order rationale:
///   1. HPF / LPF — shape the source spectrum before non-linear stages so
///      the saturator responds to a cleaner input and doesn't smear
///      distortion into bands we would just filter out anyway.
///   2. Saturation — tanh drive after filtering keeps the induced
///      harmonics inside the passband.
///   3. Bit-crush — quantizes the driven waveform; running it after
///      saturation lets the crush interact with the already-shaped
///      dynamic curve for that "vox-caster" edge on consonants.
///   4. AM (tremolo/motor) — post-saturation so the motor buzz rides
///      on the already-processed spectrum, not the pre-crunch source.
fn apply_fx_chain(buf: &mut [f32], sample_rate: u32, p: &FxParams) {
    if buf.is_empty() {
        return;
    }
    let nyquist = (sample_rate as f32) * 0.5;
    if p.hpf_hz > 0.0 && p.hpf_hz < nyquist {
        apply_one_pole_hpf(buf, sample_rate, p.hpf_hz);
    }
    if p.lpf_hz > 0.0 && p.lpf_hz < nyquist {
        apply_one_pole_lpf(buf, sample_rate, p.lpf_hz);
    }
    if p.drive_db > 0.0 {
        apply_saturation(buf, p.drive_db);
    }
    if p.crush_bits > 0.0 && p.crush_bits < 16.0 {
        apply_bit_crush(buf, p.crush_bits);
    }
    if p.am_rate_hz > 0.0 && p.am_depth > 0.0 {
        apply_amplitude_mod(buf, sample_rate, p.am_rate_hz, p.am_depth);
    }
}

/// Standard 1-pole low-pass IIR:
///   y[n] = y[n-1] + α · (x[n] − y[n-1])
/// with α = dt / (RC + dt), RC = 1 / (2π · fc).
fn apply_one_pole_lpf(buf: &mut [f32], sample_rate: u32, fc: f32) {
    let dt = 1.0 / sample_rate as f32;
    let rc = 1.0 / (2.0 * std::f32::consts::PI * fc);
    let alpha = dt / (rc + dt);
    let mut y = 0.0f32;
    for s in buf.iter_mut() {
        y += alpha * (*s - y);
        *s = y;
    }
}

/// Standard 1-pole high-pass IIR (complement of the LPF above):
///   y[n] = α · (y[n-1] + x[n] − x[n-1])
/// with α = RC / (RC + dt).
fn apply_one_pole_hpf(buf: &mut [f32], sample_rate: u32, fc: f32) {
    let dt = 1.0 / sample_rate as f32;
    let rc = 1.0 / (2.0 * std::f32::consts::PI * fc);
    let alpha = rc / (rc + dt);
    let mut prev_x = 0.0f32;
    let mut prev_y = 0.0f32;
    for s in buf.iter_mut() {
        let x = *s;
        let y = alpha * (prev_y + x - prev_x);
        prev_x = x;
        prev_y = y;
        *s = y;
    }
}

/// tanh soft-clipper. Drives the input by `10^(drive_db/20)` before the
/// non-linearity, then normalizes so a "unity" DC signal of ±1 lands
/// back near ±1 — keeps average level roughly matched as drive rises.
fn apply_saturation(buf: &mut [f32], drive_db: f32) {
    let drive = 10f32.powf(drive_db / 20.0);
    let norm = drive.tanh().max(1e-6);
    for s in buf.iter_mut() {
        *s = (*s * drive).tanh() / norm;
    }
}

/// Uniform quantizer at `bits` effective bit depth. Rounds to the nearest
/// step of `1 / (2^(bits-1))` and clamps into ±1 so a driven signal past
/// unity doesn't wrap around instead of clipping.
fn apply_bit_crush(buf: &mut [f32], bits: f32) {
    let b = bits.clamp(1.0, 16.0);
    let steps = 2f32.powf(b - 1.0);
    for s in buf.iter_mut() {
        let clamped = s.clamp(-1.0, 1.0);
        *s = (clamped * steps).round() / steps;
    }
}

/// Amplitude modulation: `x[n] · (1 + depth · sin(2π·rate·n/fs))`. With
/// `depth < 1` this is classic tremolo; `depth == 1` becomes a
/// pseudo-ring-mod (still preserves the fundamental voice, unlike a
/// true `x · sin(...)` ring mod that would strip the carrier).
fn apply_amplitude_mod(buf: &mut [f32], sample_rate: u32, rate_hz: f32, depth: f32) {
    let d = depth.clamp(0.0, 1.0);
    let phase_inc = 2.0 * std::f32::consts::PI * rate_hz / sample_rate as f32;
    let mut phase = 0.0f32;
    for s in buf.iter_mut() {
        let m = 1.0 + d * phase.sin();
        *s *= m;
        phase += phase_inc;
        if phase > std::f32::consts::TAU {
            phase -= std::f32::consts::TAU;
        }
    }
}

/// Decode a user-uploaded sample clip (wav / flac / ogg / mp3 — anything
/// symphonia handles), downmixed to mono, then resampled to the pipewire
/// target rate. Returns the mono `f32` buffer ready to be looped and fed
/// into the per-config effect chain.
fn decode_and_resample_sample(bytes: &[u8], target_rate: u32) -> Result<Vec<f32>, String> {
    let chunk = comfyui::decode_audio_chunk(bytes)
        .map_err(|e| format!("decode failed: {e:#}"))?;
    if chunk.samples.is_empty() {
        return Err("no audio samples in sample clip".into());
    }
    if chunk.sample_rate == target_rate {
        return Ok(chunk.samples);
    }
    let mut rs = build_resampler(chunk.sample_rate, target_rate)
        .map_err(|e| format!("resampler build failed: {e:#}"))?;
    resample(&mut rs, &chunk.samples).map_err(|e| format!("resample failed: {e:#}"))
}

/// Tile a source buffer to a target frame count. Empty source or zero
/// target returns empty. Longer-than-target truncates. In between, the
/// source is repeated head-to-tail until the target length is reached —
/// suitable for atmospheric drones (motor buzz, servo whine) where the
/// user-supplied clip is a short loopable texture rather than a one-shot.
fn loop_to_length(src: Vec<f32>, target_len: usize) -> Vec<f32> {
    if src.is_empty() || target_len == 0 {
        return Vec::new();
    }
    if src.len() >= target_len {
        return src[..target_len].to_vec();
    }
    let mut out = Vec::with_capacity(target_len);
    while out.len() < target_len {
        let need = target_len - out.len();
        let take = need.min(src.len());
        out.extend_from_slice(&src[..take]);
    }
    out
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
