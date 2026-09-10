//! Local HTTP API.
//!
//! Two synthesis paths sharing the same TTS backend. Both accept a **voice
//! profile** — a list of one or more `configs`, each carrying its own
//! speaker/language/instruct + per-voice DSP (pitch, detune, time, pan,
//! gain, delay). A profile with N configs synthesizes N voices in the same
//! call and mixes them into one stereo output (hive-mind / crowd).
//!
//!   POST /say                  { text, configs: [{...}, ...] }
//!                              → { ok, frames } — pushes to the virtual
//!                              mic; used by Scenes.
//!   GET    /script                                   → full script state
//!                              (turns + blocks + takes) for the default
//!                              script.
//!   POST   /script            { text } → { turn } — append a user turn,
//!                              call the LLM, parse `<speak>` blocks, return
//!                              the new assistant turn (blocks present, no
//!                              takes yet — the client kicks a per-block
//!                              /takes render).
//!   DELETE /script            → 200 — clear every turn (and cascade the
//!                              per-block widgets/WAVs off disk).
//!   POST   /script/blocks/:id/takes  { configs? } → { take, widget }
//!                              — synthesize a fresh take for the block's
//!                              stored text; auto-selects it.
//!   PATCH  /script/blocks/:id  { selectedTake }     → 200 — pin the block's
//!                              chosen take by ord.
//!   DELETE /script/takes/:id                        → 200 — drop a take
//!                              (cascades the widget + WAV).
//!   POST /widgets              { text, configs: [{...}, ...] }
//!                              → audio/wav (stereo) — synthesize, persist
//!                              a new widget row + WAV in the store,
//!                              return audio.
//!   PUT  /widgets/:id          { text, configs: [{...}, ...] }
//!                              → audio/wav — re-synthesize, overwrite the
//!                              widget's row + WAV file.
//!   PATCH /widgets/:id         { text }
//!                              → 200 — update just the widget's text
//!                              column without re-synthesizing. Used to
//!                              correct an STT transcript on a recorded
//!                              clip while leaving the audio intact.
//!   DELETE /widgets/:id                             → 200 — drop the DB
//!                              row and the WAV file (idempotent).
//!   POST /widgets/:id/say                            → { ok, frames } —
//!                              play the cached WAV through the virtual mic.
//!                              Blocks until playback finishes so clients can
//!                              sequence per-clip calls without extra timing.
//!   POST /playback/stop                              → { ok } — cancel
//!                              any in-flight TTS playback. Flushes the
//!                              pipewire ring on the next process cycle so
//!                              audio actually stops (unlike aborting the
//!                              /say fetch, which leaves queued samples
//!                              draining out for up to `ringbuf_seconds`).
//!   POST /clicks/start · /clicks/stop                → { ok } — toggle
//!                              the wait-fill "computer is thinking" click
//!                              bed. Bursts are procedural (see
//!                              [`crate::tts::clicks`]) and share the TTS
//!                              ring with speech, so any real playback
//!                              preempts filler via the same stop-gen path.
//!   POST /widgets/record       { widget_id?, text? } → { session_id } —
//!                              begins recording from the `-vox` companion
//!                              sink into an in-memory buffer.
//!   POST /widgets/record/:sid/stop                   → { id, sampleRate,
//!                              durationMs } — stops the session, encodes the
//!                              captured PCM as mono 16-bit WAV, and creates
//!                              (or updates, when a widget_id was supplied at
//!                              /record start) the widget row + WAV file.
//!   DELETE /widgets/record/:sid                      → 200 — cancels a
//!                              recording session without persisting anything.
//!   GET  /monitor.ws                                 → WebSocket that
//!                              streams the same PCM going to the pipewire
//!                              mic, encoded as 20ms Opus frames. One
//!                              encoder per subscriber; drops-on-lag. See
//!                              [`crate::monitor`].
//!   POST /scenes/mix           { scene_name, pause_ms, clip_ids }
//!                              → audio/flac (Content-Disposition attachment)
//!                              — concatenates the referenced WAVs with
//!                              silence gaps and returns a single FLAC.
//!   POST   /images             raw body + Content-Type: image/* → { id }
//!                              — stores under data/images/{id}; the client
//!                              uses `/images/{id}` as the img src.
//!   GET    /images/:id         → binary + Content-Type from the row.
//!   DELETE /images/:id         → 200; idempotent.
//!   GET    /state              → the JSON blob of projects/characters/scenes
//!                              (or `null` if unset). The client persists all
//!                              of its non-selection state here.
//!   PUT    /state              raw JSON body → 204; upserts the blob.
//!
//! All three synthesis paths go through the single-threaded TTS runner over
//! the same mpsc so backend access is serialized. `/widgets/*` additionally
//! reads/writes the sqlite + on-disk clip store from `store.rs`.

use anyhow::{Context as _, Result};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, HeaderName, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, mpsc::Sender, oneshot};
use tracing::info;
use uuid::Uuid;

use std::collections::HashMap;
use std::sync::Arc;

use crate::chat;
use crate::mixer::{AtomicMixer, MixerPatch};
use crate::monitor;
use crate::pw_source::{GraphSnapshot, PwClient, SinkRole};
use crate::script;
use crate::settings::{self, SettingsUpdate};
use crate::store::{AgentField, DEFAULT_AGENT_ID, ScriptBlockRow, ScriptTakeRow, ScriptTurnRow, Store, UpdateResult};

/// Sentinel value for an agent voice slot that the user has explicitly
/// silenced. Distinguished from `NULL` (which means "unset — use the DSP
/// fallback") so custom agents can opt out of synthesis for a role while
/// the built-in Default agent's zero-config behavior stays intact. The
/// client dropdown surfaces it as `— None (silence) —`; character UUIDs
/// never collide with this literal.
pub(crate) const VOICE_SILENCE: &str = "__silence__";
use crate::stt::SttHandle;
use tokio::sync::broadcast;

/// Ceiling for /images POST bodies. Kept above the client's own 10 MB cap
/// so a slightly-off client sees a friendly server-side error rather than a
/// silent tokio hangup.
const IMAGE_MAX_BYTES: usize = 12 * 1024 * 1024;

/// Ceiling for /state PUT bodies. Character/scene JSON stays well under
/// this — image bytes are stored separately under /images and the state
/// blob only carries their `/images/{id}` src URLs.
const STATE_MAX_BYTES: usize = 4 * 1024 * 1024;
use crate::tts::{self, Command, PlayPcmRequest, SayRequest, SynthesizeRequest, VoiceConfig};
use crate::workflow::{self, Registry};

/// Built by build.rs (`pnpm run build` in crates/rpg_vox/web) into
/// crates/rpg_vox/dist-ui/. Baked into the binary at compile time.
#[derive(RustEmbed)]
#[folder = "dist-ui/"]
struct Ui;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) tts: Sender<Command>,
    pub(crate) pw: PwClient,
    pub(crate) settings: settings::Shared,
    pub(crate) registry: Arc<Registry>,
    pub(crate) chat: chat::Client,
    pub(crate) store: Store,
    /// Fan-out for the browser monitor tap. `subscribe()` at connect time
    /// yields a `Receiver<Arc<[f32]>>`; the sender lives inside the tts
    /// runner so each Sink push mirrors into it (see `tts::Sink::push`).
    pub(crate) monitor_tap: broadcast::Sender<Arc<[f32]>>,
    /// Sample rate the monitor tap runs at (matches the pipewire target
    /// rate). Held in state so the WS handler can bail early if a future
    /// config picks a non-Opus rate.
    pub(crate) monitor_sample_rate: u32,
    /// Shared mixer atomics. `/mixer` reads a snapshot; `PUT /mixer`
    /// applies a partial patch. Also persisted to sqlite on every update.
    pub(crate) mixer: Arc<AtomicMixer>,
    /// Fan-out for the `-vox` companion sink's PCM tap. Interleaved stereo
    /// f32 at [`Self::vox_sample_rate`]. Used by `/widgets/record` to
    /// capture a widget clip directly from the vox channel instead of TTS.
    pub(crate) vox_tap: broadcast::Sender<Arc<[f32]>>,
    /// Sample rate for the vox tap (matches the pipewire target rate).
    pub(crate) vox_sample_rate: u32,
    /// In-flight recording sessions keyed by session id. Populated by
    /// `POST /widgets/record`; consumed by the matching /stop or /cancel.
    pub(crate) recordings: Arc<Mutex<HashMap<Uuid, RecordingSession>>>,
    /// Loaded SenseVoice recognizer, or `None` when STT is disabled/missing.
    /// Consulted by `/widgets/record/:sid/stop` to auto-fill the widget's
    /// caption with a transcript of what was captured; when `None`, the
    /// captured WAV is still persisted and the client's provided text is
    /// used as-is.
    pub(crate) stt: Option<SttHandle>,
}

/// One in-flight `/widgets/record` capture. The recording task lives on the
/// tokio runtime; `stop_tx` signals it to finish (draining the vox tap into
/// the final WAV), and `result_rx` delivers the accumulated stereo PCM.
pub(crate) struct RecordingSession {
    /// Widget metadata the client wants persisted with the recording.
    widget_id: Option<String>,
    text: String,
    /// Sent to the recording task to signal "stop and hand back the samples".
    stop_tx: oneshot::Sender<()>,
    /// Recording task's join handle — held so /cancel can abort the task
    /// without waiting on it (dropping the handle would detach, not stop).
    task: tokio::task::JoinHandle<()>,
    /// Delivered once by the recording task with the accumulated interleaved
    /// stereo f32 samples. Read only from the /stop handler.
    result_rx: oneshot::Receiver<Vec<f32>>,
}

/// One voice recipe in the request payload. All fields are optional; missing
/// numerics fall back to identity values, missing speaker/language/instruct
/// fall back to the backend's startup defaults. Field names use camelCase
/// to match the browser payload verbatim.
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ConfigBody {
    #[serde(default)]
    speaker: Option<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    instruct: Option<String>,
    #[serde(default)]
    pitch_semitones: Option<f32>,
    #[serde(default)]
    time_ratio: Option<f32>,
    #[serde(default)]
    detune_cents: Option<f32>,
    #[serde(default)]
    pan: Option<f32>,
    #[serde(default)]
    gain_db: Option<f32>,
    #[serde(default)]
    delay_ms: Option<f32>,
    #[serde(default)]
    hpf_hz: Option<f32>,
    #[serde(default)]
    lpf_hz: Option<f32>,
    #[serde(default)]
    drive_db: Option<f32>,
    #[serde(default)]
    crush_bits: Option<f32>,
    #[serde(default)]
    am_rate_hz: Option<f32>,
    #[serde(default)]
    am_depth: Option<f32>,
}

impl ConfigBody {
    fn into_voice_config(self) -> VoiceConfig {
        // Client-supplied ConfigBody is always preset-mode. Clone and
        // Design modes are only reachable through character voice
        // profiles, resolved by `resolve_character_configs` below with
        // store access — clients don't carry voice-file bytes over the
        // wire.
        VoiceConfig {
            mode: crate::tts::SynthMode::Preset,
            speaker: trim_opt(self.speaker),
            language: trim_opt(self.language),
            instruct: trim_opt(self.instruct),
            pitch_semitones: self.pitch_semitones.unwrap_or(0.0),
            time_ratio: self.time_ratio.unwrap_or(1.0),
            detune_cents: self.detune_cents.unwrap_or(0.0),
            pan: self.pan.unwrap_or(0.0),
            gain_db: self.gain_db.unwrap_or(0.0),
            delay_ms: self.delay_ms.unwrap_or(0.0),
            hpf_hz: self.hpf_hz.unwrap_or(0.0),
            lpf_hz: self.lpf_hz.unwrap_or(0.0),
            drive_db: self.drive_db.unwrap_or(0.0),
            crush_bits: self.crush_bits.unwrap_or(0.0),
            am_rate_hz: self.am_rate_hz.unwrap_or(0.0),
            am_depth: self.am_depth.unwrap_or(0.0),
        }
    }
}

#[derive(Debug, Deserialize)]
struct SayBody {
    text: String,
    /// Voice profile — one or more layered configs. Empty (or omitted)
    /// falls back to a single default config, matching pre-profile behavior.
    #[serde(default)]
    configs: Vec<ConfigBody>,
    /// Which project's TTS dictionary to apply before synthesis. Omitted =
    /// no substitution (external callers with no project context).
    #[serde(default, rename = "projectId")]
    project_id: Option<String>,
}

/// Coerce empty/whitespace-only strings to `None` so downstream defaults win.
fn trim_opt(s: Option<String>) -> Option<String> {
    s.and_then(|v| {
        let t = v.trim().to_string();
        (!t.is_empty()).then_some(t)
    })
}

/// Turn the request-side `configs` array into a `Vec<VoiceConfig>` ready for
/// the tts runner. An empty array is coerced to `[VoiceConfig::default()]`
/// so profile-less callers (chat auto-speak, curl) still get a synth.
fn configs_or_default(configs: Vec<ConfigBody>) -> Vec<VoiceConfig> {
    if configs.is_empty() {
        vec![VoiceConfig::default()]
    } else {
        configs.into_iter().map(ConfigBody::into_voice_config).collect()
    }
}

/// Default voice profile for a Script speech block based on its role.
/// "character" → the backend's default voice unchanged (the PC's own line).
/// "narrator"  → the same voice with a DSP shift so the storyteller reads
/// noticeably different from the PC even when only one Piper model is
/// loaded: deeper (pitch −3 semitones), slower (0.9×), slightly quieter
/// (−2 dB) and panned right so both voices don't compete on the same
/// side of the stereo image. Not a "real" second voice — a full multi-voice
/// setup would want a separate model per role — but enough to make the
/// two channels feel like different people at zero configuration.
fn default_configs_for_role(role: &str) -> Vec<VoiceConfig> {
    match role {
        "narrator" => vec![VoiceConfig {
            pitch_semitones: -3.0,
            time_ratio: 0.9,
            gain_db: -2.0,
            pan: 0.35,
            ..VoiceConfig::default()
        }],
        _ => vec![VoiceConfig::default()],
    }
}

/// Voice profile for the "GM proxy": the synthesized reading of a user
/// turn's text when the user typed instead of recording. Panned LEFT (so
/// it doesn't collide with the narrator's right-panned position), pitched
/// UP a bit and read at slightly faster-than-normal cadence so the GM
/// reads as its own distinct third voice on a single-model piper install.
fn gm_proxy_configs() -> Vec<VoiceConfig> {
    vec![VoiceConfig {
        pitch_semitones: 2.0,
        time_ratio: 1.05,
        gain_db: -1.0,
        pan: -0.3,
        ..VoiceConfig::default()
    }]
}

/// Resolve a character id to the `VoiceConfig`s of one of its voice
/// profiles. Reads the app_state JSON blob, walks `characters[]`, and
/// selects a profile: `profile_id = Some(id)` picks that specific profile
/// (used by the Characters-page Test field so each profile is auditable
/// independently); `None` falls back to `voiceProfiles[0]` (the default
/// profile every character starts with, used by Script/Scene playback
/// paths that don't carry a per-profile selection).
///
/// The profile-level `mode` dictates which SynthMode variant we build,
/// per-config `description` / `voiceFileId` fields supply the mode-specific
/// data.
///
/// Returns `None` if the character can't be found (deleted, wrong project,
/// or app_state hasn't been written yet), if its profile list is empty, if
/// a requested `profile_id` doesn't exist, if any clone-mode config's voice
/// file bytes fail to load, or on any JSON parse failure — callers fall
/// back to their hardcoded DSP preset in that case, so a stale slot never
/// blocks synthesis.
/// Look up a character's `projectId` from the app_state JSON. Used to
/// derive the dictionary project for widget renders that carry a
/// `character_id` but no explicit `projectId`. Returns `None` when the
/// character (or the app_state blob) is missing / unparseable.
async fn character_project_id(state: &AppState, character_id: &str) -> Option<String> {
    let raw = state.store.get_app_state().await.ok().flatten()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    value
        .get("characters")?
        .as_array()?
        .iter()
        .find(|c| c.get("id").and_then(|v| v.as_str()) == Some(character_id))?
        .get("projectId")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Read a project's TTS dictionary from the app_state JSON. Missing project
/// or malformed entries yield an empty list — dictionary substitution is a
/// best-effort transform, never a synth-blocking failure.
async fn load_project_dictionary(
    state: &AppState,
    project_id: &str,
) -> Vec<(String, String)> {
    let Some(raw) = state.store.get_app_state().await.ok().flatten() else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    let Some(projects) = value.get("projects").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let Some(project) = projects
        .iter()
        .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(project_id))
    else {
        return Vec::new();
    };
    let Some(entries) = project.get("dictionary").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|e| {
            let word = e.get("word").and_then(|v| v.as_str())?.trim().to_string();
            let pron = e.get("pronunciation").and_then(|v| v.as_str())?.trim().to_string();
            if word.is_empty() || pron.is_empty() {
                None
            } else {
                Some((word, pron))
            }
        })
        .collect()
}

/// Case-insensitive whole-word substitution over `text`. Every dictionary
/// entry `(word, pronunciation)` replaces occurrences of `word` (bounded by
/// non-alphanumeric characters or string edges) with `pronunciation`
/// verbatim. Longest words are matched first so a longer entry wins over a
/// shorter prefix. Preserves the surrounding punctuation and spacing.
fn apply_dictionary_substitutions(text: &str, entries: &[(String, String)]) -> String {
    if entries.is_empty() || text.is_empty() {
        return text.to_string();
    }
    let mut prepared: Vec<(String, Vec<char>, &str)> = entries
        .iter()
        .filter_map(|(w, p)| {
            let w_trim = w.trim();
            let p_trim = p.trim();
            if w_trim.is_empty() || p_trim.is_empty() {
                return None;
            }
            Some((
                w_trim.to_lowercase(),
                w_trim.chars().collect(),
                p_trim,
            ))
        })
        .collect();
    if prepared.is_empty() {
        return text.to_string();
    }
    prepared.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < chars.len() {
        let at_boundary = i == 0 || !chars[i - 1].is_alphanumeric();
        let mut matched = false;
        if at_boundary {
            for (lower_word, wchars, pron) in &prepared {
                let end = i + wchars.len();
                if end > chars.len() {
                    continue;
                }
                let slice: String = chars[i..end].iter().collect::<String>().to_lowercase();
                if slice != *lower_word {
                    continue;
                }
                let after_boundary = end == chars.len() || !chars[end].is_alphanumeric();
                if !after_boundary {
                    continue;
                }
                out.push_str(pron);
                i = end;
                matched = true;
                break;
            }
        }
        if !matched {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Apply the project's TTS dictionary to `text`. Missing project id or an
/// empty dictionary short-circuits to the original text. Called immediately
/// before every synth command so LLM prompts (built from the same source
/// text) never see the phonetic respellings.
async fn apply_project_dictionary(
    state: &AppState,
    project_id: Option<&str>,
    text: String,
) -> String {
    let Some(pid) = project_id.filter(|s| !s.is_empty()) else {
        return text;
    };
    let entries = load_project_dictionary(state, pid).await;
    apply_dictionary_substitutions(&text, &entries)
}

async fn resolve_character_configs(
    state: &AppState,
    character_id: &str,
    profile_id: Option<&str>,
) -> Option<Vec<VoiceConfig>> {
    let raw = state.store.get_app_state().await.ok().flatten()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let character = value
        .get("characters")?
        .as_array()?
        .iter()
        .find(|c| c.get("id").and_then(|v| v.as_str()) == Some(character_id))?;
    let profiles = character.get("voiceProfiles")?.as_array()?;
    let profile = match profile_id {
        Some(id) => profiles
            .iter()
            .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(id))?,
        None => profiles.first()?,
    };
    // Mode moved from profile-level to per-config in v6. Legacy profiles
    // still carry `profile.mode` (single string) and their configs have no
    // `mode` field; when we see that shape we fall back to the profile
    // mode as the default for every config, so an old preset profile
    // continues to render as preset without a migration write.
    let legacy_profile_mode = profile
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("presets");
    let configs = profile.get("configs")?.as_array()?;

    let mut out: Vec<VoiceConfig> = Vec::with_capacity(configs.len());
    for c in configs {
        let language = c
            .get("language")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty());
        // DSP knobs live on every config regardless of mode — clone/design/
        // copy outputs still get pitch-shifted / pan'd / delayed just like
        // preset.
        let read_f = |key: &str, default: f32| -> f32 {
            c.get(key).and_then(|v| v.as_f64()).map(|v| v as f32).unwrap_or(default)
        };
        let pitch_semitones = read_f("pitchSemitones", 0.0);
        let time_ratio      = read_f("timeRatio",      1.0);
        let detune_cents    = read_f("detuneCents",    0.0);
        let pan             = read_f("pan",            0.0);
        let gain_db         = read_f("gainDb",         0.0);
        let delay_ms        = read_f("delayMs",        0.0);
        let hpf_hz          = read_f("hpfHz",          0.0);
        let lpf_hz          = read_f("lpfHz",          0.0);
        let drive_db        = read_f("driveDb",        0.0);
        let crush_bits      = read_f("crushBits",      0.0);
        let am_rate_hz      = read_f("amRateHz",       0.0);
        let am_depth        = read_f("amDepth",        0.0);
        // Per-config mode wins over the legacy profile-level mode. Any
        // unknown string falls back to the legacy profile mode, which in
        // turn defaults to "presets" above.
        let mode_str = c
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or(legacy_profile_mode);
        let (mode, speaker, instruct) = match mode_str {
            "clone" => {
                let voice_id = c.get("voiceFileId").and_then(|v| v.as_str())?;
                let (bytes, filename) = state.store.get_voice_bytes(voice_id.to_string()).await.ok()??;
                (
                    crate::tts::SynthMode::Clone {
                        voice_file_bytes: bytes,
                        voice_file_name: filename,
                    },
                    None,
                    None,
                )
            }
            "design" => {
                let description = c
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .filter(|s| !s.is_empty())?;
                (
                    crate::tts::SynthMode::Design { description },
                    None,
                    None,
                )
            }
            "copy" => {
                // 0-based index on the wire; synthesize_profile validates
                // the range and rejects self-copy / cycles. Missing field
                // defaults to voice 1 (index 0), matching the UI's default
                // dropdown selection.
                let from_index = c
                    .get("copyFromIndex")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize)
                    .unwrap_or(0);
                (
                    crate::tts::SynthMode::Copy { from_index },
                    None,
                    None,
                )
            }
            "sample" => {
                // Load the raw sample bytes here so the backend never
                // touches the store. Missing / unknown / undeletable
                // sample id → drop the whole profile resolution (return
                // None from the outer fn) so the caller sees a clear 400
                // instead of a mysteriously-silent Sample layer.
                let sample_id = c.get("sampleFileId").and_then(|v| v.as_str())?;
                let (bytes, filename) = state
                    .store
                    .get_sample_bytes(sample_id.to_string())
                    .await
                    .ok()??;
                (
                    crate::tts::SynthMode::Sample {
                        sample_bytes: bytes,
                        sample_file_name: filename,
                    },
                    None,
                    None,
                )
            }
            // "presets" (default) — legacy JSON that pre-dates the mode
            // field also lands here since we defaulted to "presets" above.
            _ => (
                crate::tts::SynthMode::Preset,
                c.get("speaker")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .filter(|s| !s.is_empty()),
                c.get("instruct")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .filter(|s| !s.is_empty()),
            ),
        };
        out.push(VoiceConfig {
            mode,
            speaker,
            language,
            instruct,
            pitch_semitones,
            time_ratio,
            detune_cents,
            pan,
            gain_db,
            delay_ms,
            hpf_hz,
            lpf_hz,
            drive_db,
            crush_bits,
            am_rate_hz,
            am_depth,
        });
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Render + persist a widget that speaks `text` in the User-slot voice.
/// When the agent has a `voice_user` character set, we use that character's
/// first voice profile; otherwise we fall back to the hardcoded GM-proxy
/// DSP preset. Used by /scripts/:id/user when the user typed a message
/// without an attached mic recording — this ensures every turn in the
/// transcript is playable via Play All. On synth failure we return `None`
/// so the send still succeeds; the user just doesn't get a play button on
/// that turn.
async fn synth_gm_proxy_widget(
    state: &AppState,
    text: String,
    user_voice_character: Option<&str>,
    project_id: Option<&str>,
) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    // Explicit silence sentinel wins: skip synth entirely so the user
    // turn lands with no attached widget (no play button, nothing to
    // hear on Play All). Distinct from `None` which means "use the
    // hardcoded GM-proxy DSP".
    if user_voice_character == Some(VOICE_SILENCE) {
        return None;
    }
    let configs = match user_voice_character {
        Some(cid) => resolve_character_configs(state, cid, None)
            .await
            .unwrap_or_else(gm_proxy_configs),
        None => gm_proxy_configs(),
    };
    let clip = match render_clip(state, text.clone(), configs, project_id).await {
        Ok(c) => c,
        Err((code, msg)) => {
            tracing::warn!(%code, msg, "user-voice synth failed; user turn will have no audio");
            return None;
        }
    };
    match state
        .store
        .create_widget(text, None, clip.sample_rate, clip.duration_ms, clip.wav)
        .await
    {
        Ok(id) => Some(id),
        Err(err) => {
            tracing::warn!(err = %format!("{err:#}"), "GM proxy widget insert failed");
            None
        }
    }
}

#[derive(Debug, Serialize)]
struct SayResponse {
    ok: bool,
    frames: Option<usize>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MonitorBody {
    sink_id: u32,
}

#[derive(Debug, Serialize)]
struct ActionResponse {
    ok: bool,
    error: Option<String>,
}

pub async fn serve(
    bind: String,
    tts: Sender<Command>,
    pw: PwClient,
    settings: settings::Shared,
    registry: Arc<Registry>,
    chat: chat::Client,
    store: Store,
    monitor_tap: broadcast::Sender<Arc<[f32]>>,
    monitor_sample_rate: u32,
    mixer: Arc<AtomicMixer>,
    vox_tap: broadcast::Sender<Arc<[f32]>>,
    vox_sample_rate: u32,
    stt: Option<SttHandle>,
) -> Result<()> {
    let state = AppState {
        tts,
        pw,
        settings,
        registry,
        chat,
        store,
        monitor_tap,
        monitor_sample_rate,
        mixer,
        vox_tap,
        vox_sample_rate,
        recordings: Arc::new(Mutex::new(HashMap::new())),
        stt,
    };
    let app = Router::new()
        .route("/", get(index))
        .route("/assets/*path", get(asset_handler))
        .route("/say", post(say_handler))
        .route("/widgets", post(widget_create_handler))
        .route(
            "/widgets/:id",
            axum::routing::get(widget_get_handler)
                .put(widget_update_handler)
                .patch(widget_patch_handler)
                .delete(widget_delete_handler),
        )
        .route("/widgets/:id/say", post(widget_say_handler))
        .route("/playback/stop", post(playback_stop_handler))
        .route("/clicks/start", post(clicks_start_handler))
        .route("/clicks/stop", post(clicks_stop_handler))
        .route("/widgets/record", post(widget_record_start_handler))
        .route(
            "/widgets/record/:session_id",
            axum::routing::delete(widget_record_cancel_handler),
        )
        .route(
            "/widgets/record/:session_id/stop",
            post(widget_record_stop_handler),
        )
        .route("/scenes/mix", post(scene_mix_handler))
        .route(
            "/images",
            post(image_upload_handler).layer(DefaultBodyLimit::max(IMAGE_MAX_BYTES)),
        )
        .route(
            "/images/:id",
            axum::routing::get(image_get_handler).delete(image_delete_handler),
        )
        // Voice-prompt files (compact speaker embeddings from Qwen3 /save_prompt).
        // POST accepts multipart form-data (reference wav + metadata) and does the
        // Gradio roundtrip; body limit sized for a 30s reference at 48kHz stereo
        // wav (~5 MB), well above the 3-second minimum Qwen3-TTS needs.
        .route(
            "/voices",
            post(voice_create_handler).layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route("/voices/:id", axum::routing::delete(voice_delete_handler))
        .route(
            "/voices/:id/reference",
            axum::routing::get(voice_reference_handler),
        )
        // Raw sample clips for SynthMode::Sample. Same 8 MB cap as voices;
        // stores the bytes verbatim without any Qwen3 processing so any
        // codec symphonia can decode (wav / flac / ogg / mp3) is accepted.
        .route(
            "/samples",
            post(sample_create_handler).layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route("/samples/:id", axum::routing::delete(sample_delete_handler))
        .route(
            "/samples/:id/audio",
            axum::routing::get(sample_audio_handler),
        )
        .route(
            "/state",
            get(state_get_handler)
                .put(state_put_handler)
                .layer(DefaultBodyLimit::max(STATE_MAX_BYTES)),
        )
        .route("/healthz", get(|| async { "ok" }))
        .route("/monitor.ws", get(monitor::ws_handler))
        .route("/pw/graph", get(graph_handler))
        .route(
            "/pw/monitor",
            post(monitor_start_handler).delete(monitor_stop_handler),
        )
        .route(
            "/pw/sources/:id/link",
            post(link_source_handler),
        )
        .route(
            "/pw/sources/:id/unlink",
            post(unlink_source_handler),
        )
        .route("/mixer", get(get_mixer).put(put_mixer))
        .route("/mixer/levels", get(get_mixer_levels))
        .route("/settings", get(get_settings).post(update_settings))
        .route("/workflows", get(list_workflows))
        .route("/workflow/verify", post(verify_workflow))
        .route("/workflow/warmup", post(warmup_workflow))
        .route("/scripts", get(scripts_list).post(scripts_create))
        .route(
            "/scripts/:id",
            axum::routing::get(script_get)
                .patch(scripts_rename)
                .delete(scripts_delete),
        )
        .route("/scripts/:id/turns", axum::routing::delete(script_reset))
        .route("/scripts/:id/user", post(script_send_user))
        .route(
            "/scripts/:id/turns/:turn_id/edit-user",
            post(script_edit_user),
        )
        .route("/scripts/:id/reply", post(script_send_reply))
        .route("/scripts/:id/title", post(scripts_generate_title))
        .route("/scripts/:id/mix.flac", get(script_mix_handler))
        .route(
            "/script/blocks/:id",
            axum::routing::patch(script_block_patch),
        )
        .route(
            "/script/blocks/:id/takes",
            post(script_block_add_take),
        )
        .route(
            "/script/takes/:id",
            axum::routing::delete(script_take_delete),
        )
        .route("/agents", get(agents_list).post(agents_create))
        .route(
            "/agents/:id",
            axum::routing::get(agents_get)
                .put(agents_update)
                .delete(agents_delete),
        )
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    info!(%bind, "HTTP server listening");
    axum::serve(listener, app)
        .await
        .context("HTTP server error")?;
    Ok(())
}

async fn index() -> Response {
    match Ui::get("index.html") {
        Some(file) => (
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            file.data.into_owned(),
        )
            .into_response(),
        // dist-ui/ empty means the build didn't produce anything (e.g. someone
        // set RPG_VOX_SKIP_UI_BUILD without prebuilding). Give a useful hint
        // instead of a bare 404.
        None => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "SPA not embedded. Run `just build-ui` or unset RPG_VOX_SKIP_UI_BUILD.",
        )
            .into_response(),
    }
}

async fn asset_handler(Path(path): Path<String>) -> Response {
    let full = format!("assets/{path}");
    match Ui::get(&full) {
        Some(file) => {
            let mime = file.metadata.mimetype();
            (
                [
                    (header::CONTENT_TYPE, mime),
                    // Vite emits content-hashed filenames — safe to cache forever.
                    (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
                ],
                file.data.into_owned(),
            )
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn graph_handler(State(state): State<AppState>) -> impl IntoResponse {
    match state.pw.snapshot().await {
        Ok(snap) => (StatusCode::OK, Json(serde_json::to_value(&snap).unwrap())).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ActionResponse {
                ok: false,
                error: Some(err),
            }),
        )
            .into_response(),
    }
}

async fn monitor_start_handler(
    State(state): State<AppState>,
    Json(body): Json<MonitorBody>,
) -> impl IntoResponse {
    match state.pw.start_monitor(body.sink_id).await {
        Ok(()) => (
            StatusCode::OK,
            Json(ActionResponse {
                ok: true,
                error: None,
            }),
        )
            .into_response(),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(ActionResponse {
                ok: false,
                error: Some(err),
            }),
        )
            .into_response(),
    }
}

async fn monitor_stop_handler(State(state): State<AppState>) -> impl IntoResponse {
    match state.pw.stop_monitor().await {
        Ok(()) => (
            StatusCode::OK,
            Json(ActionResponse {
                ok: true,
                error: None,
            }),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ActionResponse {
                ok: false,
                error: Some(err),
            }),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct LinkSourceBody {
    /// "music" or "vox" — the companion sink to route this producer into.
    target: String,
}

async fn link_source_handler(
    State(state): State<AppState>,
    Path(id): Path<u32>,
    Json(body): Json<LinkSourceBody>,
) -> impl IntoResponse {
    let Some(role) = SinkRole::from_str(&body.target) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ActionResponse {
                ok: false,
                error: Some(format!("unknown target `{}` (expected \"music\" or \"vox\")", body.target)),
            }),
        )
            .into_response();
    };
    match state.pw.link_source(id, role).await {
        Ok(()) => (
            StatusCode::OK,
            Json(ActionResponse { ok: true, error: None }),
        )
            .into_response(),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(ActionResponse {
                ok: false,
                error: Some(err),
            }),
        )
            .into_response(),
    }
}

async fn unlink_source_handler(
    State(state): State<AppState>,
    Path(id): Path<u32>,
) -> impl IntoResponse {
    match state.pw.unlink_source(id).await {
        Ok(()) => (
            StatusCode::OK,
            Json(ActionResponse { ok: true, error: None }),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ActionResponse {
                ok: false,
                error: Some(err),
            }),
        )
            .into_response(),
    }
}

async fn get_mixer(State(state): State<AppState>) -> impl IntoResponse {
    (StatusCode::OK, Json(state.mixer.snapshot()))
}

/// VU-meter poll endpoint. Each call fetch-and-resets the recent peak
/// atomics so the returned values are the peak magnitudes seen since the
/// previous poll — miss a tick and you lose that window's data, which is
/// the right trade for meter animation.
async fn get_mixer_levels(State(state): State<AppState>) -> impl IntoResponse {
    (StatusCode::OK, Json(state.mixer.take_levels()))
}

/// Apply a partial mixer patch and persist the resulting snapshot. Persist
/// happens after the atomics are updated so the on-disk row always matches
/// what the pw thread is reading.
async fn put_mixer(
    State(state): State<AppState>,
    Json(patch): Json<MixerPatch>,
) -> impl IntoResponse {
    state.mixer.apply(patch);
    let snap = state.mixer.snapshot();
    if let Err(err) = state.store.put_mixer(snap).await {
        tracing::warn!(err = %format!("{err:#}"), "persisting mixer state failed");
    }
    (StatusCode::OK, Json(snap)).into_response()
}

async fn get_settings(State(state): State<AppState>) -> impl IntoResponse {
    let s = state.settings.read().await.public();
    (StatusCode::OK, Json(s))
}

async fn update_settings(
    State(state): State<AppState>,
    Json(update): Json<SettingsUpdate>,
) -> impl IntoResponse {
    let mut guard = state.settings.write().await;
    match guard.apply(update, &state.registry) {
        Ok(()) => {
            let s = guard.public();
            drop(guard);
            info!("settings updated");
            (StatusCode::OK, Json(serde_json::to_value(&s).unwrap())).into_response()
        }
        Err(err) => {
            drop(guard);
            (
                StatusCode::BAD_REQUEST,
                Json(ActionResponse {
                    ok: false,
                    error: Some(err),
                }),
            )
                .into_response()
        }
    }
}

#[derive(Debug, Serialize)]
struct WorkflowInfo {
    name: String,
    summary: workflow::WorkflowSummary,
}

async fn list_workflows(State(state): State<AppState>) -> impl IntoResponse {
    let items: Vec<WorkflowInfo> = state
        .registry
        .entries()
        .map(|e| WorkflowInfo {
            name: e.name.clone(),
            summary: e.summary.clone(),
        })
        .collect();
    (StatusCode::OK, Json(items))
}

#[derive(Debug, Serialize)]
struct VerifyResponse {
    ok: bool,
    missing: Vec<String>,
    error: Option<String>,
}

async fn verify_workflow(State(state): State<AppState>) -> impl IntoResponse {
    let (base, classes) = {
        let s = state.settings.read().await;
        (
            s.comfyui_base.clone(),
            s.workflow_summary.node_classes.clone(),
        )
    };
    let http = reqwest::Client::new();
    match workflow::missing_nodes(&http, &base, &classes).await {
        Ok(missing) => (
            StatusCode::OK,
            Json(VerifyResponse {
                ok: missing.is_empty(),
                missing,
                error: None,
            }),
        )
            .into_response(),
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            Json(VerifyResponse {
                ok: false,
                missing: vec![],
                error: Some(format!("{err:#}")),
            }),
        )
            .into_response(),
    }
}

async fn warmup_workflow(State(state): State<AppState>) -> impl IntoResponse {
    let (base, wf) = {
        let s = state.settings.read().await;
        (s.comfyui_base.clone(), s.workflow_json.clone())
    };
    match tts::warmup(&base, &wf).await {
        Ok(()) => (
            StatusCode::OK,
            Json(ActionResponse {
                ok: true,
                error: None,
            }),
        )
            .into_response(),
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            Json(ActionResponse {
                ok: false,
                error: Some(format!("{err:#}")),
            }),
        )
            .into_response(),
    }
}

// GraphSnapshot only needs to be visible to justify the import used above.
#[allow(dead_code)]
fn _snapshot_type_hint(_: GraphSnapshot) {}

async fn say_handler(
    State(state): State<AppState>,
    Json(body): Json<SayBody>,
) -> impl IntoResponse {
    let text = body.text.trim().to_string();
    if text.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(SayResponse {
                ok: false,
                frames: None,
                error: Some("empty text".into()),
            }),
        )
            .into_response();
    }
    let text = apply_project_dictionary(&state, body.project_id.as_deref(), text).await;
    let configs = configs_or_default(body.configs);

    let (reply_tx, reply_rx) = oneshot::channel();
    if state
        .tts
        .send(Command::Say(SayRequest {
            text,
            configs,
            reply: reply_tx,
        }))
        .await
        .is_err()
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(SayResponse {
                ok: false,
                frames: None,
                error: Some("tts task gone".into()),
            }),
        )
            .into_response();
    }

    match reply_rx.await {
        Ok(Ok(frames)) => (
            StatusCode::OK,
            Json(SayResponse {
                ok: true,
                frames: Some(frames),
                error: None,
            }),
        )
            .into_response(),
        Ok(Err(err)) => (
            StatusCode::BAD_GATEWAY,
            Json(SayResponse {
                ok: false,
                frames: None,
                error: Some(err),
            }),
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(SayResponse {
                ok: false,
                frames: None,
                error: Some("tts task dropped reply channel".into()),
            }),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// /script — chat-style conversation whose assistant turns can carry inline
// `<speak>…</speak>` blocks. Each block gets its own persistent row and one
// or more "takes" (widget clips). The client renders the assistant content
// as markdown, splicing an inline audio widget in place of each block.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct ScriptTake {
    id: String,
    ord: i64,
    widget_id: String,
    /// Cached clip metadata copied from the widget row so the client can size
    /// its progress UI without a second /widgets/:id round trip. `None` when
    /// the widget row was already gone (shouldn't happen in practice, but the
    /// FK cascade could race with delete).
    sample_rate: Option<u32>,
    duration_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
struct ScriptBlock {
    id: String,
    ord: i64,
    text: String,
    /// "narrator" or "character". Frontend styles the two pill types
    /// differently; server-side voice profile defaults are role-driven too.
    role: String,
    selected_take: Option<i64>,
    takes: Vec<ScriptTake>,
}

#[derive(Debug, Serialize)]
struct ScriptTurn {
    id: String,
    ord: i64,
    role: String,
    content: String,
    /// Widget attached to this turn — non-null only for user turns entered
    /// via the mic (voice memo → transcript path). Frontend shows a small
    /// play button next to the transcript that streams the widget through
    /// the pipewire virtual mic on click.
    #[serde(skip_serializing_if = "Option::is_none")]
    widget_id: Option<String>,
    blocks: Vec<ScriptBlock>,
}

#[derive(Debug, Serialize)]
struct ScriptView {
    id: String,
    turns: Vec<ScriptTurn>,
}

async fn take_view(store: &Store, row: ScriptTakeRow) -> ScriptTake {
    let (sample_rate, duration_ms) = match store.get_widget(row.widget_id.clone()).await {
        Ok(Some(w)) => (Some(w.sample_rate), Some(w.duration_ms)),
        _ => (None, None),
    };
    ScriptTake {
        id: row.id,
        ord: row.ord,
        widget_id: row.widget_id,
        sample_rate,
        duration_ms,
    }
}

async fn block_view(store: &Store, row: ScriptBlockRow) -> ScriptBlock {
    let mut takes = Vec::with_capacity(row.takes.len());
    for t in row.takes {
        takes.push(take_view(store, t).await);
    }
    ScriptBlock {
        id: row.id,
        ord: row.ord,
        text: row.text,
        role: row.role,
        selected_take: row.selected_take,
        takes,
    }
}

async fn turn_view(store: &Store, row: ScriptTurnRow) -> ScriptTurn {
    let mut blocks = Vec::with_capacity(row.blocks.len());
    for b in row.blocks {
        blocks.push(block_view(store, b).await);
    }
    ScriptTurn {
        id: row.id,
        ord: row.ord,
        role: row.role,
        content: row.content,
        widget_id: row.widget_id,
        blocks,
    }
}

#[derive(Debug, Serialize)]
struct ScriptSummary {
    id: String,
    name: String,
    created_at: i64,
    updated_at: i64,
}

fn summary_view(row: crate::store::ScriptSummaryRow) -> ScriptSummary {
    ScriptSummary {
        id: row.id,
        name: row.name,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

async fn scripts_list(State(state): State<AppState>) -> Response {
    match state.store.list_scripts().await {
        Ok(rows) => {
            let out: Vec<ScriptSummary> = rows.into_iter().map(summary_view).collect();
            (StatusCode::OK, Json(out)).into_response()
        }
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), "store: scripts list failed");
            (StatusCode::INTERNAL_SERVER_ERROR, format!("store: {err:#}")).into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
struct ScriptCreateBody {
    #[serde(default)]
    name: Option<String>,
}

async fn scripts_create(
    State(state): State<AppState>,
    Json(body): Json<ScriptCreateBody>,
) -> Response {
    let name = body
        .name
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "New script".to_string());
    match state.store.create_script(name).await {
        Ok(row) => (StatusCode::CREATED, Json(summary_view(row))).into_response(),
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), "store: script create failed");
            (StatusCode::INTERNAL_SERVER_ERROR, format!("store: {err:#}")).into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
struct ScriptRenameBody {
    name: String,
}

async fn scripts_rename(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ScriptRenameBody>,
) -> Response {
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty name").into_response();
    }
    match state.store.rename_script(id.clone(), name).await {
        Ok(UpdateResult::Updated) => {
            (StatusCode::OK, Json(ActionResponse { ok: true, error: None })).into_response()
        }
        Ok(UpdateResult::NotFound) => {
            (StatusCode::NOT_FOUND, format!("no script {id}")).into_response()
        }
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), %id, "store: script rename failed");
            (StatusCode::INTERNAL_SERVER_ERROR, format!("store: {err:#}")).into_response()
        }
    }
}

async fn scripts_delete(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let widget_ids = match state.store.delete_script(id.clone()).await {
        Ok(v) => v,
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), %id, "store: script delete failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("store: {err:#}"),
            )
                .into_response();
        }
    };
    for wid in widget_ids {
        if let Err(err) = state.store.delete_widget(wid.clone()).await {
            tracing::warn!(id = %wid, err = %format!("{err:#}"),
                "widget cleanup failed on script delete");
        }
    }
    (StatusCode::OK, Json(ActionResponse { ok: true, error: None })).into_response()
}

/// Generate a short LLM-derived title from a script's existing turns and
/// persist it as the script's name. Client fires this fire-and-forget
/// after the first assistant reply lands; server-side we're a bit strict
/// about the output (single line, <= 60 chars, quotes stripped) so a
/// chatty model can't paste a paragraph in as the title.
async fn scripts_generate_title(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let script_row = match state.store.get_script(id.clone()).await {
        Ok(r) => r,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("store: {err:#}"),
            )
                .into_response();
        }
    };
    if script_row.turns.is_empty() {
        return (StatusCode::BAD_REQUEST, "script has no turns yet").into_response();
    }

    // Build a tiny "sample" of the conversation for the title prompt so
    // huge histories don't cost a fortune to summarize. First user turn +
    // first assistant turn is usually enough context to pick a title.
    let mut sample: Vec<chat::ChatMessage> = Vec::new();
    for t in script_row.turns.iter().take(4) {
        sample.push(chat::ChatMessage {
            role: t.role.clone(),
            content: t.content.clone(),
        });
    }
    // Overriding the system prompt entirely so the story's own prompt
    // (which may tell the model to reply in-character with <speak> tags)
    // doesn't leak into the title.
    let title_prompt = concat!(
        "You are naming a chat conversation. Given the following exchange, ",
        "reply with ONLY a short title (3–6 words, no quotes, no punctuation ",
        "at the end, no <speak> tags, no preamble). The title should reflect ",
        "the scene or topic. Reply with the title only, no explanation."
    );

    let raw = match state
        .chat
        .generate_reply(sample, Some(title_prompt.to_string()))
        .await
    {
        Ok(t) => t,
        Err(err) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("llm: {err:#}"),
            )
                .into_response();
        }
    };

    // Post-process: single line, strip quotes / <speak> junk / trailing
    // punctuation, cap length.
    let cleaned = raw
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '“' || c == '”')
        .replace("<speak>", "")
        .replace("</speak>", "")
        .trim()
        .trim_end_matches(|c: char| c == '.' || c == '!' || c == '?')
        .to_string();
    let name = if cleaned.chars().count() > 60 {
        let truncated: String = cleaned.chars().take(60).collect();
        truncated.trim_end().to_string()
    } else {
        cleaned
    };
    if name.is_empty() {
        return (StatusCode::BAD_GATEWAY, "LLM produced an empty title").into_response();
    }

    if let Err(err) = state.store.rename_script(id.clone(), name.clone()).await {
        tracing::error!(err = %format!("{err:#}"), %id, "store: rename after title-gen failed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("store: {err:#}"),
        )
            .into_response();
    }

    (StatusCode::OK, Json(serde_json::json!({ "name": name }))).into_response()
}

async fn script_get(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let row = match state.store.get_script(id.clone()).await {
        Ok(r) => r,
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), "store: script get failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("store: {err:#}"),
            )
                .into_response();
        }
    };
    let mut turns = Vec::with_capacity(row.turns.len());
    for t in row.turns {
        turns.push(turn_view(&state.store, t).await);
    }
    (
        StatusCode::OK,
        Json(ScriptView {
            id: row.id,
            turns,
        }),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
struct ScriptSendUserBody {
    text: String,
    /// Optional widget id (from a `/widgets/record` stop) to attach to the
    /// user turn. When present, the frontend uses it to render a play
    /// button next to the user's message so the original audio is
    /// replayable. When ABSENT, the server synthesizes a User-voice proxy
    /// clip so every turn is playable.
    #[serde(default, rename = "widgetId")]
    widget_id: Option<String>,
    /// Which agent to consult for the User voice slot. Omitted → use the
    /// Default agent (which has no slot set → the hardcoded GM-proxy DSP).
    #[serde(default, rename = "agentId")]
    agent_id: Option<String>,
    /// Project whose TTS dictionary applies to the GM-proxy synth. Client
    /// sends the currently loaded project; server falls back to the
    /// agent's own `project_id` when absent.
    #[serde(default, rename = "projectId")]
    project_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ScriptSendReplyBody {
    /// Which stored agent's `system_prompt` to use for the LLM call. See
    /// `script_send_user` for how the picker propagates through the flow.
    #[serde(default, rename = "agentId")]
    agent_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct ScriptUserResponse {
    user_turn: ScriptTurn,
}

#[derive(Debug, Serialize)]
struct ScriptReplyResponse {
    assistant_turn: ScriptTurn,
}

/// Phase 1 of the /script two-step: persist the user's turn (with an
/// attached recorded widget or a freshly-synthesized GM voice proxy) and
/// push the text into the chat client's rolling history so the phase-2
/// LLM call sees it. Returns as soon as the GM synth (or the trivial
/// no-synth path) finishes — before the LLM has been called at all — so
/// the client can begin playing the GM proxy while the reply is still
/// being generated.
async fn script_send_user(
    State(state): State<AppState>,
    Path(script_id): Path<String>,
    Json(body): Json<ScriptSendUserBody>,
) -> Response {
    let text = body.text.trim().to_string();
    if text.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty text").into_response();
    }

    // Resolve the agent's User voice slot up front — a missing agent or an
    // unresolvable character both fall back to the hardcoded GM-proxy DSP
    // so nothing about voice-slot lookups can block the send path. Also
    // grab the agent's project_id as the dictionary fallback when the
    // client didn't send an explicit projectId.
    let agent_row = match body.agent_id.clone().filter(|s| !s.is_empty()) {
        Some(aid) => state.store.get_agent(aid).await.ok().flatten(),
        None => None,
    };
    let user_voice_character: Option<String> =
        agent_row.as_ref().and_then(|a| a.voice_user.clone());
    let project_id: Option<String> = body
        .project_id
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| agent_row.as_ref().and_then(|a| a.project_id.clone()));
    let user_widget_id = match body.widget_id.clone() {
        Some(id) => Some(id),
        None => {
            synth_gm_proxy_widget(
                &state,
                text.clone(),
                user_voice_character.as_deref(),
                project_id.as_deref(),
            )
            .await
        }
    };

    let user_row = match state
        .store
        .create_turn(
            script_id.clone(),
            "user".into(),
            text.clone(),
            user_widget_id,
            Vec::new(),
        )
        .await
    {
        Ok(r) => r,
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), "store: user turn insert failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("store: {err:#}"),
            )
                .into_response();
        }
    };
    // No more in-memory history push: the chat client is stateless, so
    // the /reply handler rebuilds history from stored turns on demand.

    let user_turn = turn_view(&state.store, user_row).await;
    (
        StatusCode::OK,
        Json(ScriptUserResponse { user_turn }),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
struct ScriptEditUserBody {
    text: String,
    /// Same semantics as [`ScriptSendUserBody::agent_id`] — picks the User
    /// voice slot for the freshly synthesized GM proxy widget.
    #[serde(default, rename = "agentId")]
    agent_id: Option<String>,
    /// Same semantics as [`ScriptSendUserBody::project_id`] — picks the
    /// TTS dictionary applied to the fresh GM proxy synth.
    #[serde(default, rename = "projectId")]
    project_id: Option<String>,
}

/// Edit a user turn: truncate the transcript from that turn onward
/// (dropping every later turn's blocks / takes / attached widgets) and
/// insert a REPLACEMENT user turn with the new text. Semantically this
/// is "rewind history to before this turn, then re-send it with different
/// wording" — the client fires `POST /scripts/:id/reply` next to let the
/// LLM generate a fresh response against the new history.
///
/// The original turn's attached widget (recording or old GM proxy) can't
/// match the new text anymore, so we always synth a fresh GM proxy for
/// the replacement. Same voice-slot lookup as [`script_send_user`] — an
/// agent with a User character override wins, otherwise the hardcoded
/// GM DSP; the silence sentinel produces a widget-less turn.
async fn script_edit_user(
    State(state): State<AppState>,
    Path((script_id, turn_id)): Path<(String, String)>,
    Json(body): Json<ScriptEditUserBody>,
) -> Response {
    let text = body.text.trim().to_string();
    if text.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty text").into_response();
    }

    // Truncate atomically so a racing /reply against the pre-edit history
    // can't slip in between the delete and the re-insert. `None` = the
    // turn belongs to a different script (or was already deleted); tell
    // the caller instead of silently inserting a floating new turn.
    let cleanup = match state
        .store
        .truncate_script_from_turn(script_id.clone(), turn_id.clone())
        .await
    {
        Ok(Some(ids)) => ids,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, format!("no turn {turn_id}")).into_response();
        }
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), %script_id, %turn_id,
                "store: truncate at turn failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("store: {err:#}"),
            )
                .into_response();
        }
    };
    // Fire-and-forget WAV cleanup — same as script_reset. A failed unlink
    // just leaves an orphan file; the DB rows are already gone.
    for wid in cleanup {
        if let Err(err) = state.store.delete_widget(wid.clone()).await {
            tracing::warn!(id = %wid, err = %format!("{err:#}"),
                "widget cleanup failed on user-turn edit");
        }
    }

    // Same voice-slot resolution + GM proxy synth as script_send_user's
    // no-widget path. Silence sentinel intentionally omitted from the
    // agent lookup (the slot lookup returns the sentinel string, which
    // synth_gm_proxy_widget then honors by returning None).
    let agent_row = match body.agent_id.clone().filter(|s| !s.is_empty()) {
        Some(aid) => state.store.get_agent(aid).await.ok().flatten(),
        None => None,
    };
    let user_voice_character: Option<String> =
        agent_row.as_ref().and_then(|a| a.voice_user.clone());
    let project_id: Option<String> = body
        .project_id
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| agent_row.as_ref().and_then(|a| a.project_id.clone()));
    let user_widget_id = synth_gm_proxy_widget(
        &state,
        text.clone(),
        user_voice_character.as_deref(),
        project_id.as_deref(),
    )
    .await;

    let user_row = match state
        .store
        .create_turn(
            script_id.clone(),
            "user".into(),
            text.clone(),
            user_widget_id,
            Vec::new(),
        )
        .await
    {
        Ok(r) => r,
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), "store: edited user turn insert failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("store: {err:#}"),
            )
                .into_response();
        }
    };

    let user_turn = turn_view(&state.store, user_row).await;
    (StatusCode::OK, Json(ScriptUserResponse { user_turn })).into_response()
}

/// Phase 2 of the /script two-step: call the LLM against the history the
/// phase-1 handler just pushed to, parse the reply into narrator/character
/// regions, persist the assistant turn, and return it. Failure here leaves
/// the user turn in place — the client can retry this handler without
/// re-sending the user message.
async fn script_send_reply(
    State(state): State<AppState>,
    Path(script_id): Path<String>,
    Json(body): Json<ScriptSendReplyBody>,
) -> Response {
    let want_agent_id = body
        .agent_id
        .clone()
        .unwrap_or_else(|| DEFAULT_AGENT_ID.to_string());
    let agent_prompt: Option<String> = match state.store.get_agent(want_agent_id.clone()).await {
        Ok(Some(a)) => Some(a.system_prompt),
        Ok(None) => match state.store.get_agent(DEFAULT_AGENT_ID.to_string()).await {
            Ok(Some(a)) => Some(a.system_prompt),
            _ => None,
        },
        Err(err) => {
            tracing::warn!(err = %format!("{err:#}"), "agent lookup failed; using default");
            None
        }
    };

    // Reload history from the stored turns rather than a stale in-memory
    // copy — this is what makes multi-script work with a single chat client.
    let history: Vec<chat::ChatMessage> = match state.store.get_script(script_id.clone()).await {
        Ok(row) => row
            .turns
            .into_iter()
            .map(|t| chat::ChatMessage {
                role: t.role,
                content: t.content,
            })
            .collect(),
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), "store: history rebuild failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("store: {err:#}"),
            )
                .into_response();
        }
    };
    let reply = match state.chat.generate_reply(history, agent_prompt).await {
        Ok(r) => r,
        Err(err) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("llm: {err:#}"),
            )
                .into_response();
        }
    };

    let regions: Vec<(String, String)> = script::extract_script_regions(&reply)
        .into_iter()
        .map(|(role, text)| (role.as_str().to_string(), text))
        .collect();
    let assistant_row = match state
        .store
        .create_turn(
            script_id.clone(),
            "assistant".into(),
            reply,
            None,
            regions,
        )
        .await
    {
        Ok(r) => r,
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), "store: assistant turn insert failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("store: {err:#}"),
            )
                .into_response();
        }
    };
    let assistant_turn = turn_view(&state.store, assistant_row).await;

    (
        StatusCode::OK,
        Json(ScriptReplyResponse { assistant_turn }),
    )
        .into_response()
}

async fn script_reset(State(state): State<AppState>, Path(script_id): Path<String>) -> Response {
    let widget_ids = match state
        .store
        .clear_script(script_id.clone())
        .await
    {
        Ok(v) => v,
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), "store: script clear failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ActionResponse {
                    ok: false,
                    error: Some(format!("{err:#}")),
                }),
            )
                .into_response();
        }
    };
    // The FK cascade drops script_speech_blocks + script_speech_takes when
    // the turn row goes, but the underlying `widgets` rows are NOT cascaded
    // (the takes → widgets FK is `ON DELETE CASCADE` in the widget→take
    // direction, not the other way). User-turn widgets are attached via
    // `widget_id ON DELETE SET NULL`, so those also stay. Delete each
    // widget explicitly here — that drops the row AND the WAV file, so
    // Play All doesn't try to replay a dangling clip after a reset.
    for wid in widget_ids {
        if let Err(err) = state.store.delete_widget(wid.clone()).await {
            tracing::warn!(id = %wid, err = %format!("{err:#}"),
                "widget cleanup failed on script reset");
        }
    }
    (StatusCode::OK, Json(ActionResponse { ok: true, error: None })).into_response()
}

#[derive(Debug, Deserialize)]
struct ScriptAddTakeBody {
    #[serde(default)]
    configs: Vec<ConfigBody>,
    /// Which agent's voice slot to consult when `configs` is empty. Absent
    /// → fall through to the hardcoded role-based DSP preset.
    #[serde(default, rename = "agentId")]
    agent_id: Option<String>,
    /// Which project's TTS dictionary to apply to the block text before
    /// synthesis. Absent → falls back to the agent's own `project_id`, and
    /// finally to no substitution when neither is known (Default agent).
    #[serde(default, rename = "projectId")]
    project_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct ScriptAddTakeResponse {
    take: ScriptTake,
    /// Echo of the freshly minted widget's clip metadata so the client can
    /// wire up its progress bar without a second /widgets/:id call.
    sample_rate: u32,
    duration_ms: u64,
}

async fn script_block_add_take(
    State(state): State<AppState>,
    Path(block_id): Path<String>,
    Json(body): Json<ScriptAddTakeBody>,
) -> Response {
    let info = match state.store.get_block_info(block_id.clone()).await {
        Ok(Some(i)) => i,
        Ok(None) => return (StatusCode::NOT_FOUND, format!("no block {block_id}")).into_response(),
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), %block_id, "store: block lookup failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("store: {err:#}"),
            )
                .into_response();
        }
    };
    let text = info.text;

    // Fetch the agent once — its voice slot picks the config, and its
    // `project_id` is the dictionary fallback when the client didn't send
    // an explicit `projectId` in the body.
    let agent_row = match body.agent_id.clone().filter(|s| !s.is_empty()) {
        Some(aid) => state.store.get_agent(aid).await.ok().flatten(),
        None => None,
    };

    // Client-supplied configs win when non-empty (so scene-style profile
    // overrides still work). When absent, look up the agent's voice slot
    // for this block's role and use that character's default profile;
    // fall through to the hardcoded DSP preset when either the agent has
    // no slot set or the referenced character can't be resolved.
    let configs = if body.configs.is_empty() {
        let slot_character: Option<String> = agent_row
            .as_ref()
            .and_then(|a| match info.role.as_str() {
                "narrator" => a.voice_narrator.clone(),
                _ => a.voice_character.clone(),
            });
        // Silence sentinel: caller (usually a manual ⟳ click) still gets
        // a take rendered, but with the hardcoded DSP fallback rather than
        // the silenced character. Auto-render / Play All are gated
        // client-side so those paths honor the silence naturally.
        match slot_character.as_deref() {
            Some(VOICE_SILENCE) | None => default_configs_for_role(&info.role),
            Some(cid) => resolve_character_configs(&state, cid, None)
                .await
                .unwrap_or_else(|| default_configs_for_role(&info.role)),
        }
    } else {
        configs_or_default(body.configs)
    };
    let project_id = body
        .project_id
        .clone()
        .or_else(|| agent_row.as_ref().and_then(|a| a.project_id.clone()));
    let persist_instruct = configs.first().and_then(|c| c.instruct.clone());
    let clip = match render_clip(&state, text.clone(), configs, project_id.as_deref()).await {
        Ok(c) => c,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let widget_id = match state
        .store
        .create_widget(
            text,
            persist_instruct,
            clip.sample_rate,
            clip.duration_ms,
            clip.wav.clone(),
        )
        .await
    {
        Ok(id) => id,
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), "store: create widget failed for take");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("store: {err:#}"),
            )
                .into_response();
        }
    };
    let take_row = match state
        .store
        .append_take(block_id.clone(), widget_id.clone())
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            // Block vanished between the text lookup and the take insert.
            // Best-effort cleanup of the widget we just made so we don't
            // strand the WAV file.
            let _ = state.store.delete_widget(widget_id).await;
            return (StatusCode::NOT_FOUND, format!("no block {block_id}")).into_response();
        }
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), %block_id, "store: append take failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("store: {err:#}"),
            )
                .into_response();
        }
    };

    // FIFO cap: keep only the 3 most recent takes per block. The `ord`
    // column still increments monotonically (append_take does
    // `MAX(ord) + 1`), so the visible take numbers keep growing — the
    // user can tell they're auditioning e.g. take #7 even though only
    // takes #5, #6, #7 remain. Cleanup deletes both the DB row and the
    // widget's WAV file so the on-disk store doesn't grow unbounded.
    const MAX_TAKES_PER_BLOCK: usize = 3;
    match state
        .store
        .prune_block_takes(block_id.clone(), MAX_TAKES_PER_BLOCK)
        .await
    {
        Ok(pruned) => {
            for wid in pruned {
                if let Err(err) = state.store.delete_widget(wid.clone()).await {
                    tracing::warn!(id = %wid, err = %format!("{err:#}"),
                        "widget cleanup failed on take prune");
                }
            }
        }
        Err(err) => {
            tracing::warn!(err = %format!("{err:#}"), %block_id,
                "take prune failed; older takes may accumulate");
        }
    }

    let take = ScriptTake {
        id: take_row.id,
        ord: take_row.ord,
        widget_id: take_row.widget_id,
        sample_rate: Some(clip.sample_rate),
        duration_ms: Some(clip.duration_ms),
    };
    (
        StatusCode::OK,
        Json(ScriptAddTakeResponse {
            take,
            sample_rate: clip.sample_rate,
            duration_ms: clip.duration_ms,
        }),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
struct ScriptBlockPatchBody {
    #[serde(rename = "selectedTake")]
    selected_take: i64,
}

async fn script_block_patch(
    State(state): State<AppState>,
    Path(block_id): Path<String>,
    Json(body): Json<ScriptBlockPatchBody>,
) -> Response {
    match state
        .store
        .set_selected_take(block_id.clone(), body.selected_take)
        .await
    {
        Ok(UpdateResult::Updated) => {
            (StatusCode::OK, Json(ActionResponse { ok: true, error: None })).into_response()
        }
        Ok(UpdateResult::NotFound) => (
            StatusCode::NOT_FOUND,
            format!("no take with ord {} on block {block_id}", body.selected_take),
        )
            .into_response(),
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), %block_id, "store: selected take update failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("store: {err:#}"),
            )
                .into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// /agents — named system-prompt profiles for the /script LLM call.
// The built-in "Default" agent is seeded on boot from prompts/script.txt
// and is READ-ONLY (rejects rename + prompt edits + delete). Any other
// agent is user-created and fully editable.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct AgentView {
    id: String,
    name: String,
    system_prompt: String,
    /// Project the agent belongs to. Null for the built-in Default (which
    /// shows up in every project's picker).
    project_id: Option<String>,
    /// Character ids feeding each voice slot. Null = fall back to the
    /// hardcoded DSP preset for that role.
    voice_user: Option<String>,
    voice_narrator: Option<String>,
    voice_character: Option<String>,
    /// Wait-fill "computer is thinking" click preset id. Null = no click
    /// bed during the LLM wait. Currently the only non-null value the
    /// server accepts is "vintage" — see [`crate::tts::clicks::ClickPreset`].
    interstitial: Option<String>,
    /// Client uses this to hide edit/delete UI on the Default agent.
    read_only: bool,
    created_at: i64,
    updated_at: i64,
}

fn agent_view(row: crate::store::AgentRow) -> AgentView {
    let read_only = row.id == DEFAULT_AGENT_ID;
    AgentView {
        id: row.id,
        name: row.name,
        system_prompt: row.system_prompt,
        project_id: row.project_id,
        voice_user: row.voice_user,
        voice_narrator: row.voice_narrator,
        voice_character: row.voice_character,
        interstitial: row.interstitial,
        read_only,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

#[derive(Debug, Deserialize)]
struct AgentListQuery {
    /// When set, the returned list is filtered to agents in that project
    /// PLUS the built-in Default (which has NULL project_id and is always
    /// visible). Omitted → every agent, for admin/debug callers.
    #[serde(default, rename = "projectId")]
    project_id: Option<String>,
}

async fn agents_list(
    State(state): State<AppState>,
    Query(q): Query<AgentListQuery>,
) -> Response {
    let filter = q.project_id.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    match state.store.list_agents(filter).await {
        Ok(rows) => {
            let out: Vec<AgentView> = rows.into_iter().map(agent_view).collect();
            (StatusCode::OK, Json(out)).into_response()
        }
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), "store: agents list failed");
            (StatusCode::INTERNAL_SERVER_ERROR, format!("store: {err:#}")).into_response()
        }
    }
}

async fn agents_get(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.store.get_agent(id.clone()).await {
        Ok(Some(row)) => (StatusCode::OK, Json(agent_view(row))).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, format!("no agent {id}")).into_response(),
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), %id, "store: agent get failed");
            (StatusCode::INTERNAL_SERVER_ERROR, format!("store: {err:#}")).into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
struct AgentCreateBody {
    name: String,
    /// Project the new agent belongs to. Nullable so an admin/CLI caller
    /// could create a global agent, but the UI always passes the current
    /// project id.
    #[serde(default, rename = "projectId")]
    project_id: Option<String>,
}

async fn agents_create(
    State(state): State<AppState>,
    Json(body): Json<AgentCreateBody>,
) -> Response {
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty name").into_response();
    }
    let project_id = body.project_id.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    match state.store.create_agent(name, project_id).await {
        Ok(row) => (StatusCode::CREATED, Json(agent_view(row))).into_response(),
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), "store: agent create failed");
            (StatusCode::INTERNAL_SERVER_ERROR, format!("store: {err:#}")).into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
struct AgentUpdateBody {
    #[serde(default)]
    name: Option<String>,
    /// Prompt body — client sends this on every autosave keystroke (with
    /// debouncing), so allow empty strings (a fresh Create leaves it "").
    #[serde(default, rename = "systemPrompt")]
    system_prompt: Option<String>,
    /// Voice slots. Outer `Option` = "leave alone"; inner `Option` = the
    /// value the caller wants to set (Some(id) = pick that character;
    /// None = clear the slot back to the DSP-preset fallback). Uses
    /// `deserialize_with` to distinguish "field absent" from "field: null".
    #[serde(default, deserialize_with = "deserialize_some", rename = "voiceUser")]
    voice_user: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_some", rename = "voiceNarrator")]
    voice_narrator: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_some", rename = "voiceCharacter")]
    voice_character: Option<Option<String>>,
    /// Interstitial click preset id. Same `Option<Option<String>>` shape
    /// as the voice slots: outer None = leave alone, `Some(None)` = clear
    /// to NULL, `Some(Some("vintage"))` = set the preset. Unknown preset
    /// strings are rejected in [`agents_update`] with a 400 so client
    /// typos surface immediately.
    #[serde(default, deserialize_with = "deserialize_some")]
    interstitial: Option<Option<String>>,
}

/// `Option<Option<T>>` serde helper: distinguishes an absent field (outer
/// None → leave the DB column alone) from `field: null` (outer Some(None) →
/// clear to NULL). Standard trick — serde's default `Option` deserialize
/// treats both as None, which would prevent clients from ever clearing a
/// nullable column.
fn deserialize_some<'de, T, D>(deserializer: D) -> Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    T::deserialize(deserializer).map(Some)
}

async fn agents_update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<AgentUpdateBody>,
) -> Response {
    // Default agent is immutable — reject with 403 so the client can show
    // a clear "can't edit Default" affordance rather than silently drop
    // the change.
    if id == DEFAULT_AGENT_ID {
        return (
            StatusCode::FORBIDDEN,
            "the Default agent is read-only — create a new agent to customize",
        )
            .into_response();
    }
    let mut changes: Vec<AgentField> = Vec::new();
    if let Some(name) = body.name {
        let trimmed = name.trim().to_string();
        if !trimmed.is_empty() {
            changes.push(AgentField::Name(trimmed));
        }
    }
    if let Some(prompt) = body.system_prompt {
        changes.push(AgentField::SystemPrompt(prompt));
    }
    // Normalize the inner Option: empty/whitespace-only strings become None
    // (= clear the slot) so a client that sends `""` gets the same fallback
    // behavior as one that sends `null`.
    let clean = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if let Some(v) = body.voice_user { changes.push(AgentField::VoiceUser(clean(v))); }
    if let Some(v) = body.voice_narrator { changes.push(AgentField::VoiceNarrator(clean(v))); }
    if let Some(v) = body.voice_character { changes.push(AgentField::VoiceCharacter(clean(v))); }
    if let Some(v) = body.interstitial {
        let cleaned = clean(v);
        // Reject unknown preset ids up-front so a client typo returns a
        // clear 400 instead of getting stored and later ignored by the
        // runtime (with the user wondering why their bed never plays).
        if let Some(preset) = cleaned.as_deref() {
            if crate::tts::clicks::ClickPreset::from_str_id(preset).is_none() {
                return (
                    StatusCode::BAD_REQUEST,
                    format!("unknown interstitial preset: {preset}"),
                )
                    .into_response();
            }
        }
        changes.push(AgentField::Interstitial(cleaned));
    }
    match state.store.update_agent(id.clone(), changes).await {
        Ok(UpdateResult::Updated) => {
            (StatusCode::OK, Json(ActionResponse { ok: true, error: None })).into_response()
        }
        Ok(UpdateResult::NotFound) => {
            (StatusCode::NOT_FOUND, format!("no agent {id}")).into_response()
        }
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), %id, "store: agent update failed");
            (StatusCode::INTERNAL_SERVER_ERROR, format!("store: {err:#}")).into_response()
        }
    }
}

async fn agents_delete(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    if id == DEFAULT_AGENT_ID {
        return (
            StatusCode::FORBIDDEN,
            "the Default agent is read-only",
        )
            .into_response();
    }
    match state.store.delete_agent(id.clone()).await {
        Ok(UpdateResult::Updated) => {
            (StatusCode::OK, Json(ActionResponse { ok: true, error: None })).into_response()
        }
        Ok(UpdateResult::NotFound) => {
            (StatusCode::NOT_FOUND, format!("no agent {id}")).into_response()
        }
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), %id, "store: agent delete failed");
            (StatusCode::INTERNAL_SERVER_ERROR, format!("store: {err:#}")).into_response()
        }
    }
}

async fn script_take_delete(
    State(state): State<AppState>,
    Path(take_id): Path<String>,
) -> Response {
    let deleted = match state.store.delete_take(take_id.clone()).await {
        Ok(Some(d)) => d,
        Ok(None) => return (StatusCode::NOT_FOUND, format!("no take {take_id}")).into_response(),
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), %take_id, "store: delete take failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("store: {err:#}"),
            )
                .into_response();
        }
    };
    // Drop the widget row + WAV file. The FK cascade would already have
    // dropped the row when we deleted the take, so this is best-effort
    // cleanup of any stray on-disk WAV. (delete_widget is idempotent on
    // missing rows.)
    if let Err(err) = state.store.delete_widget(deleted.widget_id.clone()).await {
        tracing::warn!(err = %format!("{err:#}"), widget_id = %deleted.widget_id, "widget cleanup after take delete failed");
    }
    let _ = deleted.block_id;
    (StatusCode::OK, Json(ActionResponse { ok: true, error: None })).into_response()
}

// ---------------------------------------------------------------------------
// /widgets — persistent clip design.
//
// Each render (create or update) synthesizes fresh PCM, writes it to
// `data/clips/{id}.wav`, and mirrors the widget's text+instruct into the
// sqlite row. Delete drops both. See `store.rs` for the storage details.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WidgetBody {
    text: String,
    /// Voice profile — see [`SayBody`] for shape and fallback semantics.
    /// Ignored when `character_id` is present: server-side resolution via
    /// [`resolve_character_configs`] wins so clone / design profiles land
    /// their correct SynthMode without the client having to know voice-file
    /// bytes.
    #[serde(default)]
    configs: Vec<ConfigBody>,
    /// Optional character id. When set, the server resolves this character's
    /// voice profile (see `profile_id`) and uses those configs; `configs`
    /// above is discarded. Used by the Characters-page Test field.
    #[serde(default)]
    character_id: Option<String>,
    /// Optional profile id, only meaningful alongside `character_id`. Picks
    /// which of the character's `voiceProfiles` to render; `None` falls back
    /// to the first / default profile.
    #[serde(default)]
    profile_id: Option<String>,
    /// Which project's TTS dictionary to apply. Client passes the currently
    /// selected project so raw-config renders (no character) still get the
    /// substitution. When absent and `character_id` is present, the server
    /// falls back to that character's own projectId.
    #[serde(default)]
    project_id: Option<String>,
}

/// Rendered clip in a form ready to hand to the store + client. Held as
/// `Vec<u8>` twice (once as WAV, once as the response body) — small enough
/// (~200 KB for a 2s clip) that avoiding an extra clone isn't worth
/// contorting the code.
struct RenderedClip {
    wav: Vec<u8>,
    sample_rate: u32,
    duration_ms: u64,
}

/// Push a synth request through the TTS runner, encode stereo WAV, return
/// it. The caller decides what to do with the bytes (persist + respond, in
/// the widget handlers below).
///
/// `project_id` is the project whose TTS dictionary should be applied to
/// `text` before synthesis. Pass `None` to skip substitution (external
/// callers with no project context).
async fn render_clip(
    state: &AppState,
    text: String,
    configs: Vec<VoiceConfig>,
    project_id: Option<&str>,
) -> Result<RenderedClip, (StatusCode, String)> {
    let text = apply_project_dictionary(state, project_id, text).await;
    let (reply_tx, reply_rx) = oneshot::channel();
    if state
        .tts
        .send(Command::Synthesize(SynthesizeRequest {
            text,
            configs,
            reply: reply_tx,
        }))
        .await
        .is_err()
    {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "tts task gone".into()));
    }

    let outcome = match reply_rx.await {
        Ok(Ok(o)) => o,
        Ok(Err(err)) => return Err((StatusCode::BAD_GATEWAY, err)),
        Err(_) => return Err((StatusCode::INTERNAL_SERVER_ERROR, "tts dropped reply".into())),
    };

    let duration_ms = (outcome.samples.len() as u64 * 1000) / outcome.sample_rate.max(1) as u64;
    let wav = encode_wav_pcm16_stereo(&outcome.samples, outcome.sample_rate);
    Ok(RenderedClip {
        wav,
        sample_rate: outcome.sample_rate,
        duration_ms,
    })
}

/// Parse+validate a widget body. Empty text is rejected; missing config
/// fields fall back to the backend defaults. `persist_instruct` is the
/// first config's instruct, mirrored into the row's legacy `instruct`
/// column so callers that still read that column see something sensible.
struct NormalizedWidget {
    text: String,
    configs: Vec<VoiceConfig>,
    persist_instruct: Option<String>,
    /// Project whose TTS dictionary applies to `text`. Derived from the
    /// body's explicit `projectId` first, then the character's own
    /// `projectId` when only `characterId` was sent.
    project_id: Option<String>,
}

async fn normalize_widget_body(
    state: &AppState,
    body: WidgetBody,
) -> Result<NormalizedWidget, (StatusCode, String)> {
    let text = body.text.trim().to_string();
    if text.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "empty text".into()));
    }
    // Character-scoped path: the Characters-page Test field sends
    // characterId (+profileId) so clone/design profiles synth in their
    // correct mode. A stale or dropped character id short-circuits to a
    // 400 so the client learns the profile is gone rather than silently
    // rendering a fallback voice under an unrelated profile.
    let configs = if let Some(cid) = body.character_id.as_deref() {
        match resolve_character_configs(state, cid, body.profile_id.as_deref()).await {
            Some(cfg) => cfg,
            None => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!("could not resolve character {cid} / profile {:?}", body.profile_id),
                ));
            }
        }
    } else {
        configs_or_default(body.configs)
    };
    let persist_instruct = configs.first().and_then(|c| c.instruct.clone());
    let project_id = match body.project_id.filter(|s| !s.is_empty()) {
        Some(pid) => Some(pid),
        None => match body.character_id.as_deref() {
            Some(cid) => character_project_id(state, cid).await,
            None => None,
        },
    };
    Ok(NormalizedWidget {
        text,
        configs,
        persist_instruct,
        project_id,
    })
}

/// Build the audio/wav response with the metadata headers the client uses
/// (widget id, sample rate, duration).
fn wav_response(id: &str, clip: RenderedClip) -> Response {
    (
        [
            (header::CONTENT_TYPE, "audio/wav".to_string()),
            (header::CACHE_CONTROL, "no-store".to_string()),
            (HeaderName::from_static("x-widget-id"), id.to_string()),
            (
                HeaderName::from_static("x-sample-rate"),
                clip.sample_rate.to_string(),
            ),
            (
                HeaderName::from_static("x-duration-ms"),
                clip.duration_ms.to_string(),
            ),
        ],
        Bytes::from(clip.wav),
    )
        .into_response()
}

async fn widget_create_handler(
    State(state): State<AppState>,
    Json(body): Json<WidgetBody>,
) -> Response {
    let NormalizedWidget { text, configs, persist_instruct, project_id } = match normalize_widget_body(&state, body).await {
        Ok(t) => t,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let clip = match render_clip(&state, text.clone(), configs, project_id.as_deref()).await {
        Ok(c) => c,
        Err((code, msg)) => return (code, msg).into_response(),
    };

    let id = match state
        .store
        .create_widget(
            text,
            persist_instruct,
            clip.sample_rate,
            clip.duration_ms,
            clip.wav.clone(),
        )
        .await
    {
        Ok(id) => id,
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), "store: create failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("store: {err:#}"),
            )
                .into_response();
        }
    };

    info!(%id, "widget created");
    wav_response(&id, clip)
}

async fn widget_update_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<WidgetBody>,
) -> Response {
    let NormalizedWidget { text, configs, persist_instruct, project_id } = match normalize_widget_body(&state, body).await {
        Ok(t) => t,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let clip = match render_clip(&state, text.clone(), configs, project_id.as_deref()).await {
        Ok(c) => c,
        Err((code, msg)) => return (code, msg).into_response(),
    };

    match state
        .store
        .update_widget(
            id.clone(),
            text,
            persist_instruct,
            clip.sample_rate,
            clip.duration_ms,
            clip.wav.clone(),
        )
        .await
    {
        Ok(UpdateResult::Updated) => {}
        Ok(UpdateResult::NotFound) => {
            return (StatusCode::NOT_FOUND, format!("no widget with id {id}")).into_response();
        }
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), %id, "store: update failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("store: {err:#}"),
            )
                .into_response();
        }
    }

    info!(%id, "widget updated");
    wav_response(&id, clip)
}

/// Body for `PATCH /widgets/:id`. Only the caption text is updatable
/// through this route — everything else (WAV, configs, sample rate)
/// stays as-is so an STT-corrected recording keeps its audio.
#[derive(Debug, Deserialize)]
struct WidgetPatchBody {
    text: String,
}

/// Update the widget's caption without touching audio. Empty text is
/// allowed (recordings don't require a caption, same as at record-stop time).
async fn widget_patch_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<WidgetPatchBody>,
) -> Response {
    let text = body.text.trim().to_string();
    match state.store.update_widget_text(id.clone(), text).await {
        Ok(UpdateResult::Updated) => {
            info!(%id, "widget text updated");
            (StatusCode::OK, Json(ActionResponse { ok: true, error: None })).into_response()
        }
        Ok(UpdateResult::NotFound) => {
            (StatusCode::NOT_FOUND, format!("no widget with id {id}")).into_response()
        }
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), %id, "store: text update failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ActionResponse {
                    ok: false,
                    error: Some(format!("{err:#}")),
                }),
            )
                .into_response()
        }
    }
}

/// Read a previously-rendered clip back from disk. Used by the client on
/// reload to rehydrate SpeakCells whose widgetId came out of localStorage —
/// no re-synthesis, just the cached WAV plus its metadata headers.
async fn widget_get_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let row = match state.store.get_widget(id.clone()).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, format!("no widget with id {id}"))
                .into_response();
        }
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), %id, "store: get failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("store: {err:#}"),
            )
                .into_response();
        }
    };
    let path = state.store.clip_path(&id);
    let wav = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Row exists but WAV file was lost — treat as missing so the
            // client falls back to a fresh render.
            return (StatusCode::NOT_FOUND, format!("clip file missing for {id}"))
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("clip read: {e}"),
            )
                .into_response();
        }
    };
    wav_response(
        &id,
        RenderedClip {
            wav,
            sample_rate: row.sample_rate,
            duration_ms: row.duration_ms,
        },
    )
}

async fn widget_delete_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    match state.store.delete_widget(id.clone()).await {
        Ok(()) => {
            info!(%id, "widget deleted");
            (StatusCode::OK, Json(ActionResponse { ok: true, error: None })).into_response()
        }
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), %id, "store: delete failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ActionResponse {
                    ok: false,
                    error: Some(format!("{err:#}")),
                }),
            )
                .into_response()
        }
    }
}

/// Play a previously-rendered clip through the pipewire virtual mic. Blocks
/// until the ring buffer has drained so the response lands when playback has
/// actually finished (not merely been enqueued). Clients that sequence
/// multiple clips can therefore just await one call per clip and add a
/// scene-level pause between them.
async fn widget_say_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let row = match state.store.get_widget(id.clone()).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(SayResponse {
                    ok: false,
                    frames: None,
                    error: Some(format!("no widget with id {id}")),
                }),
            )
                .into_response();
        }
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), %id, "store: get failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SayResponse {
                    ok: false,
                    frames: None,
                    error: Some(format!("store: {err:#}")),
                }),
            )
                .into_response();
        }
    };

    let path = state.store.clip_path(&id);
    let wav = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (
                StatusCode::NOT_FOUND,
                Json(SayResponse {
                    ok: false,
                    frames: None,
                    error: Some(format!("clip file missing for {id}")),
                }),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SayResponse {
                    ok: false,
                    frames: None,
                    error: Some(format!("clip read: {e}")),
                }),
            )
                .into_response();
        }
    };

    let (sample_rate, samples) = match decode_wav_pcm16_stereo_any(&wav) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(SayResponse {
                    ok: false,
                    frames: None,
                    error: Some(format!("decode {id}: {err}")),
                }),
            )
                .into_response();
        }
    };
    let _ = row; // metadata is authoritative on the row; decoded rate takes precedence.

    // Latest-wins: bump the stop-gen so any in-flight clip on the mic
    // starts bailing NOW, and claim a fresh play seq so any older PlayPcm
    // still queued in the runner mpsc skips itself on dispatch. Together
    // these give the user "click a clip → that clip plays, not-plus-a-
    // queue-of-earlier-clicks" without needing an explicit client-side
    // stopPlayback() before each play.
    state.mixer.request_tts_stop();
    let seq = state.mixer.claim_play_seq();

    let (reply_tx, reply_rx) = oneshot::channel();
    if state
        .tts
        .send(Command::PlayPcm(PlayPcmRequest {
            samples,
            sample_rate,
            seq,
            reply: reply_tx,
        }))
        .await
        .is_err()
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(SayResponse {
                ok: false,
                frames: None,
                error: Some("tts task gone".into()),
            }),
        )
            .into_response();
    }

    match reply_rx.await {
        Ok(Ok(frames)) => (
            StatusCode::OK,
            Json(SayResponse {
                ok: true,
                frames: Some(frames),
                error: None,
            }),
        )
            .into_response(),
        Ok(Err(err)) => (
            StatusCode::BAD_GATEWAY,
            Json(SayResponse {
                ok: false,
                frames: None,
                error: Some(err),
            }),
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(SayResponse {
                ok: false,
                frames: None,
                error: Some("tts task dropped reply channel".into()),
            }),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// /playback/stop — cancel any TTS clip currently playing through the mic.
//
// Aborting the `/widgets/:id/say` HTTP request alone isn't enough: by the
// time the fetch is cancelled the server has already pushed the whole clip
// into the pipewire ring buffer, so audio keeps draining out for up to
// ringbuf_seconds. This bumps `mixer.tts_stop_gen`, which two workers watch
// for: the pipewire process callback drains the TTS ring on the very next
// cycle, and the TTS runner's PlayPcm handler bails out of its push loop /
// drain wait. Result: audio is silent within ~one pipewire cycle.
// ---------------------------------------------------------------------------

async fn playback_stop_handler(State(state): State<AppState>) -> Response {
    let gen = state.mixer.request_tts_stop();
    info!(gen, "playback stop requested");
    (StatusCode::OK, Json(ActionResponse { ok: true, error: None })).into_response()
}

// ---------------------------------------------------------------------------
// /clicks/start · /clicks/stop — wait-fill "computer is thinking" click bed.
//
// The Script page flips this on at Send so Discord participants get a soft
// mechanical clicking cue while the LLM is deliberating, and flips it off
// when the first assistant speech take begins playback. The generator
// itself lives in `tts::run_clicks` and pushes bursts into the same ring
// as speech, so real playback naturally preempts filler via the existing
// tts_stop_gen path. Off-request bumps the stop-gen so any burst already
// sitting in the ring gets flushed instead of tailing off audibly.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
struct ClicksStartBody {
    /// Preset id (see [`crate::tts::clicks::ClickPreset::from_str_id`]).
    /// Omitted → default to `"vintage"`. Unknown → 400 so a typo in a
    /// client build surfaces immediately instead of silently mis-voicing.
    #[serde(default)]
    preset: Option<String>,
}

async fn clicks_start_handler(
    State(state): State<AppState>,
    body: Option<Json<ClicksStartBody>>,
) -> Response {
    let raw = body
        .and_then(|Json(b)| b.preset)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "vintage".to_string());
    let Some(preset) = crate::tts::clicks::ClickPreset::from_str_id(&raw) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ActionResponse {
                ok: false,
                error: Some(format!("unknown click preset: {raw}")),
            }),
        )
            .into_response();
    };
    state.mixer.set_clicks_preset(Some(preset));
    info!(preset = %raw, "wait-fill clicks enabled");
    (StatusCode::OK, Json(ActionResponse { ok: true, error: None })).into_response()
}

async fn clicks_stop_handler(State(state): State<AppState>) -> Response {
    // Setting the preset to `None` is enough to stop the generator from
    // emitting new bursts (see `run_clicks` — it polls this every burst
    // boundary and idles when unset), and any burst mid-push also bails
    // via `handle_click_burst`'s is_stopped which watches this same flag.
    // Deliberately do NOT bump `tts_stop_gen` here: that atomic is what
    // `handle_play_pcm`'s push loop watches for "abort me mid-clip", and
    // the Script page fires this endpoint concurrently with the very
    // first assistant PlayPcm (the activeClip effect races with
    // playWidget). Bumping was cutting the tail off the first clip on
    // fresh replies. Any burst already sitting in the ring gets flushed
    // by the NEXT PlayPcm's own request_tts_stop; a lone stop without a
    // follow-on play would tail up to ~120 ms of clicks, which no user
    // flow currently hits (manual cancel paths also fire /playback/stop
    // or /widgets/:id/say, both of which do drain the ring).
    state.mixer.set_clicks_preset(None);
    info!("wait-fill clicks disabled");
    (StatusCode::OK, Json(ActionResponse { ok: true, error: None })).into_response()
}

// ---------------------------------------------------------------------------
// /widgets/record — capture a widget clip directly from the vox tap.
//
// Two-phase HTTP flow (start / stop) so the client controls the recording
// length interactively — the user hits Record, speaks, then hits Stop. Vox
// PCM is broadcast at 48 kHz stereo f32 from the pw source callback (see
// `pw_source.rs`); the recording task subscribes on start and buffers every
// chunk until /stop signals it to hand back the accumulated samples. /cancel
// drops the session without persisting.
//
// Samples are downmixed L+R → mono and encoded as our existing mono 16-bit
// WAV so the on-disk clip is indistinguishable from a TTS-rendered widget
// for downstream consumers (widget_get, widget_say, scenes/mix).
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
struct RecordStartBody {
    /// Existing widget row to overwrite (from the SpeakCell's current
    /// widgetId). When absent the /stop handler creates a new widget row.
    #[serde(default)]
    widget_id: Option<String>,
    /// Client-supplied caption/label. May be empty — recordings don't
    /// require any text, unlike TTS renders. Stored verbatim on the row.
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Serialize)]
struct RecordStartResponse {
    session_id: String,
}

#[derive(Debug, Serialize)]
struct RecordStopResponse {
    id: String,
    sample_rate: u32,
    duration_ms: u64,
    /// Transcript produced by the SenseVoice STT pass, if enabled. `None`
    /// when STT wasn't loaded (missing model or --stt-disabled); the client
    /// then falls back to whatever it had in the caption textarea. Empty
    /// string means STT ran but produced no text (silence / non-speech).
    #[serde(skip_serializing_if = "Option::is_none")]
    transcript: Option<String>,
}

/// Longest single recording we buffer before the task auto-stops. 5 minutes
/// at 48 kHz stereo f32 is ~110 MB, which is plenty of headroom for any
/// scene clip while still capping runaway memory if a client vanishes
/// between /record and /stop.
const RECORD_MAX_FRAMES: usize = 48_000 * 60 * 5;

async fn widget_record_start_handler(
    State(state): State<AppState>,
    body: Option<Json<RecordStartBody>>,
) -> Response {
    // Body is optional so a plain `POST /widgets/record` (no JSON) works
    // from the client side; empty body just means "new widget, no text".
    let Json(body) = body.unwrap_or_else(|| Json(RecordStartBody::default()));

    let session_id = Uuid::new_v4();
    let mut vox_rx = state.vox_tap.subscribe();
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let (result_tx, result_rx) = oneshot::channel::<Vec<f32>>();

    let task = tokio::spawn(async move {
        let mut buf: Vec<f32> = Vec::new();
        tokio::pin!(stop_rx);
        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                recv = vox_rx.recv() => {
                    match recv {
                        Ok(chunk) => {
                            if buf.len() >= RECORD_MAX_FRAMES * 2 {
                                // Hit the ceiling — stop accumulating but
                                // keep the select alive so /stop still gets
                                // the buffer we've captured so far.
                                continue;
                            }
                            buf.extend_from_slice(&chunk);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            // Slow consumer — a chunk was dropped. Live
                            // recording semantics: skip it and keep going.
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
        let _ = result_tx.send(buf);
    });

    let text = body.text.unwrap_or_default();
    let session = RecordingSession {
        widget_id: body.widget_id.clone(),
        text,
        stop_tx,
        task,
        result_rx,
    };
    state.recordings.lock().await.insert(session_id, session);

    info!(%session_id, widget_id = ?body.widget_id, "recording session started");
    (
        StatusCode::OK,
        Json(RecordStartResponse {
            session_id: session_id.to_string(),
        }),
    )
        .into_response()
}

async fn widget_record_stop_handler(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Response {
    let sid = match Uuid::parse_str(&session_id) {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid session id").into_response(),
    };
    let session = match state.recordings.lock().await.remove(&sid) {
        Some(s) => s,
        None => return (StatusCode::NOT_FOUND, "no such recording session").into_response(),
    };
    let RecordingSession {
        widget_id,
        text,
        stop_tx,
        task,
        result_rx,
    } = session;

    // Signal the task to stop and await it. Ignore stop_tx send errors — if
    // the task already exited (e.g. broadcast closed), result_rx still
    // yields whatever it captured.
    let _ = stop_tx.send(());
    let stereo = match result_rx.await {
        Ok(samples) => samples,
        Err(_) => {
            // Task dropped its sender without producing a result — abort to
            // free the JoinHandle and surface a 500.
            task.abort();
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "recording task ended without result",
            )
                .into_response();
        }
    };
    // Task is done draining; join it so we don't leak the handle.
    let _ = task.await;

    // Vox tap is interleaved stereo f32 [L,R,L,R,...]. Convert to stereo
    // pairs so we can encode the widget WAV as stereo (matching the
    // TTS-render path); keep a mono downmix for the STT recognizer, which
    // only accepts mono.
    let pairs: Vec<[f32; 2]> = stereo
        .chunks_exact(2)
        .map(|c| [c[0], c[1]])
        .collect();
    if pairs.is_empty() {
        return (StatusCode::BAD_REQUEST, "recording is empty").into_response();
    }

    let sample_rate = state.vox_sample_rate;

    // Trim leading + trailing silence so a hesitant start or a slow finger
    // on Stop doesn't leave dead air on either end of the saved clip. STT
    // gets the trimmed audio too so its transcript reflects what actually
    // plays back.
    let (trim_start, trim_end) = trim_silence_range(&pairs, sample_rate);
    let trimmed: &[[f32; 2]] = &pairs[trim_start..trim_end];

    let mono_for_stt: Vec<f32> = trimmed.iter().map(|p| (p[0] + p[1]) * 0.5).collect();

    let duration_ms = (trimmed.len() as u64 * 1000) / sample_rate.max(1) as u64;

    // Transcribe on a blocking pool if STT is loaded — sherpa-onnx's decode
    // is CPU-bound (feature extraction + ONNX inference) and can take
    // hundreds of milliseconds even for short clips, so keep it off the
    // tokio reactor. The transcript overrides the client-supplied text when
    // non-empty; otherwise we fall back to what the client typed.
    let transcript: Option<String> = if let Some(recog) = state.stt.clone() {
        let mono_for_stt = mono_for_stt.clone();
        match tokio::task::spawn_blocking(move || recog.transcribe(&mono_for_stt, sample_rate))
            .await
        {
            Ok(Ok(t)) => {
                info!(
                    len = t.len(),
                    duration_ms,
                    "STT transcript"
                );
                Some(t)
            }
            Ok(Err(err)) => {
                tracing::warn!(err = %format!("{err:#}"), "STT failed; falling back to client text");
                None
            }
            Err(_) => {
                tracing::warn!("STT task panicked; falling back to client text");
                None
            }
        }
    } else {
        None
    };

    // WAV encoding is cheap (linear pass over a Vec) so we do it inline
    // after the STT pass — no benefit to overlapping the two on this
    // short clip.
    let wav = encode_wav_pcm16_stereo(trimmed, sample_rate);

    // Prefer a non-empty transcript over whatever the client typed; both
    // being empty is fine (widget row's `text` column stores "" cleanly).
    let persisted_text = match transcript.as_deref() {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => text.clone(),
    };

    let id = if let Some(existing) = widget_id.clone() {
        match state
            .store
            .update_widget(
                existing.clone(),
                persisted_text,
                None,
                sample_rate,
                duration_ms,
                wav,
            )
            .await
        {
            Ok(UpdateResult::Updated) => existing,
            Ok(UpdateResult::NotFound) => {
                return (
                    StatusCode::NOT_FOUND,
                    format!("no widget with id {existing}"),
                )
                    .into_response();
            }
            Err(err) => {
                tracing::error!(err = %format!("{err:#}"), "store: record update failed");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("store: {err:#}"),
                )
                    .into_response();
            }
        }
    } else {
        match state
            .store
            .create_widget(persisted_text, None, sample_rate, duration_ms, wav)
            .await
        {
            Ok(id) => id,
            Err(err) => {
                tracing::error!(err = %format!("{err:#}"), "store: record create failed");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("store: {err:#}"),
                )
                    .into_response();
            }
        }
    };

    info!(
        %id,
        %session_id,
        raw_frames = pairs.len(),
        trimmed_frames = trimmed.len(),
        duration_ms,
        "recording saved as widget"
    );
    (
        StatusCode::OK,
        Json(RecordStopResponse {
            id,
            sample_rate,
            duration_ms,
            transcript,
        }),
    )
        .into_response()
}

async fn widget_record_cancel_handler(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Response {
    let sid = match Uuid::parse_str(&session_id) {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid session id").into_response(),
    };
    match state.recordings.lock().await.remove(&sid) {
        Some(session) => {
            // Abort the recording task and drop its buffer without touching
            // the store. `stop_tx`/`result_rx` are dropped here too, which
            // shuts down cleanly whether the task reads them or not.
            session.task.abort();
            info!(%session_id, "recording session canceled");
            (StatusCode::OK, Json(ActionResponse { ok: true, error: None })).into_response()
        }
        None => (StatusCode::NOT_FOUND, "no such recording session").into_response(),
    }
}

// ---------------------------------------------------------------------------
// /scenes/mix — concatenate a scene's rendered clips into a single FLAC.
//
// Client sends the ordered widgetIds and the per-gap pause. The server reads
// each clip's on-disk WAV (mono 16-bit PCM at the sample rate stored on the
// row), stitches them together with `pause_ms` of silence between clips, and
// encodes the whole thing as FLAC. Sample rates are required to match across
// clips — this stays true as long as the scene was rendered by a single TTS
// backend, which is the common case. Mismatches surface a 409 so the caller
// can prompt the user to re-render.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SceneMixBody {
    scene_name: String,
    #[serde(default)]
    pause_ms: u32,
    clip_ids: Vec<String>,
}

async fn scene_mix_handler(
    State(state): State<AppState>,
    Json(body): Json<SceneMixBody>,
) -> Response {
    if body.clip_ids.is_empty() {
        return (StatusCode::BAD_REQUEST, "no clips to mix").into_response();
    }

    // Read every clip's WAV concurrently; each read is small (a few hundred
    // KB) so this is fine to spin up N tasks at once.
    let reads = body.clip_ids.iter().map(|id| {
        let path = state.store.clip_path(id);
        let id = id.clone();
        async move {
            let bytes = tokio::fs::read(&path)
                .await
                .with_context(|| format!("reading clip {id}"))?;
            Ok::<(String, Vec<u8>), anyhow::Error>((id, bytes))
        }
    });
    let clips = match futures_util::future::try_join_all(reads).await {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::NOT_FOUND,
                format!("clip missing: {err:#}"),
            )
                .into_response();
        }
    };

    // Decode each WAV into (rate, stereo pairs). Widget WAVs are stereo
    // 16-bit LE since the profile-mix rewrite, but the decoder also
    // accepts pre-existing mono clips by duplicating each sample to L=R.
    let mut decoded: Vec<(u32, Vec<[f32; 2]>)> = Vec::with_capacity(clips.len());
    for (id, bytes) in clips {
        match decode_wav_pcm16_stereo_any(&bytes) {
            Ok(pair) => decoded.push(pair),
            Err(err) => {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("decode {id}: {err}"),
                )
                    .into_response();
            }
        }
    }

    let sample_rate = decoded[0].0;
    if let Some((idx, (r, _))) = decoded.iter().enumerate().find(|(_, (r, _))| *r != sample_rate)
    {
        return (
            StatusCode::CONFLICT,
            format!(
                "clip {idx} has sample_rate {r} but scene starts at {sample_rate}; \
                 re-render the scene so every clip matches"
            ),
        )
            .into_response();
    }

    let gap_frames = ((body.pause_ms as u64 * sample_rate as u64) / 1000) as usize;
    // Concatenate. FLAC wants channel-interleaved i32 samples in host order:
    // [L0, R0, L1, R1, ...] for a 2-channel stream.
    let total_frames: usize = decoded.iter().map(|(_, s)| s.len()).sum::<usize>()
        + gap_frames * decoded.len().saturating_sub(1);
    let mut mixed: Vec<i32> = Vec::with_capacity(total_frames * 2);
    for (i, (_, pairs)) in decoded.iter().enumerate() {
        if i > 0 && gap_frames > 0 {
            // Silent gap: two i32 zeros per frame (L, R).
            mixed.extend(std::iter::repeat_n(0i32, gap_frames * 2));
        }
        for [l, r] in pairs.iter() {
            mixed.push(f32_to_i16_clipped(*l) as i32);
            mixed.push(f32_to_i16_clipped(*r) as i32);
        }
    }

    // FLAC encode. Runs on a blocking pool because encoding a several-second
    // clip can take tens of milliseconds and we don't want to stall the
    // tokio reactor.
    let flac = match tokio::task::spawn_blocking(move || encode_flac_stereo_i16(&mixed, sample_rate))
        .await
    {
        Ok(Ok(v)) => v,
        Ok(Err(err)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("flac encode: {err}"),
            )
                .into_response();
        }
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "flac encode task panicked",
            )
                .into_response();
        }
    };

    let filename = scene_mix_filename(&body.scene_name);
    info!(
        clips = body.clip_ids.len(),
        bytes = flac.len(),
        filename = %filename,
        "scene mix produced"
    );
    (
        [
            (header::CONTENT_TYPE, "audio/flac".to_string()),
            (header::CACHE_CONTROL, "no-store".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        Bytes::from(flac),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// /scripts/:id/mix.flac — concatenate a script's entire audio timeline into a
// single FLAC download.
//
// Walks turns in order and, per turn, includes:
//   * user turn: the attached widget (recording or GM proxy), if present.
//   * assistant turn: each block's SELECTED take widget in reading order.
//     Blocks with no takes yet are skipped — they'd otherwise leave dead
//     air, and the client already tracks unrendered blocks visually.
// A fixed short pause is inserted between clips so consecutive turns don't
// audibly butt up against each other. Same sample-rate constraint as
// /scenes/mix: every clip in the timeline has to match, otherwise we 409.
// ---------------------------------------------------------------------------

/// Silence between adjacent clips in the mixed .flac. Tuned to match the
/// natural rhythm of a scripted conversation: enough that turns don't
/// slur into each other, short enough that the resulting file plays
/// tight.
const SCRIPT_MIX_GAP_MS: u32 = 400;

async fn script_mix_handler(
    State(state): State<AppState>,
    Path(script_id): Path<String>,
) -> Response {
    let script = match state.store.get_script(script_id.clone()).await {
        Ok(r) => r,
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), %script_id, "store: script get failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("store: {err:#}"))
                .into_response();
        }
    };
    let script_name = state
        .store
        .get_script_name(script_id.clone())
        .await
        .ok()
        .flatten()
        .unwrap_or_default();

    // Walk turns in order, collecting the widget ids that should end up on
    // the timeline. For assistant blocks we pick the SELECTED take, falling
    // back to the last take when selection is unset (matches the Play All
    // logic client-side).
    let mut widget_ids: Vec<String> = Vec::new();
    for turn in &script.turns {
        match turn.role.as_str() {
            "user" => {
                if let Some(w) = turn.widget_id.as_ref() {
                    widget_ids.push(w.clone());
                }
            }
            "assistant" => {
                for block in &turn.blocks {
                    if block.takes.is_empty() {
                        continue;
                    }
                    let chosen = block
                        .selected_take
                        .and_then(|ord| block.takes.iter().find(|t| t.ord == ord))
                        .unwrap_or_else(|| block.takes.last().expect("non-empty checked above"));
                    widget_ids.push(chosen.widget_id.clone());
                }
            }
            _ => {}
        }
    }

    if widget_ids.is_empty() {
        return (StatusCode::NOT_FOUND, "script has no audio to mix").into_response();
    }

    // Read every clip's WAV concurrently — same shape as scene_mix_handler.
    let reads = widget_ids.iter().map(|id| {
        let path = state.store.clip_path(id);
        let id = id.clone();
        async move {
            let bytes = tokio::fs::read(&path)
                .await
                .with_context(|| format!("reading clip {id}"))?;
            Ok::<(String, Vec<u8>), anyhow::Error>((id, bytes))
        }
    });
    let clips = match futures_util::future::try_join_all(reads).await {
        Ok(v) => v,
        Err(err) => {
            return (StatusCode::NOT_FOUND, format!("clip missing: {err:#}"))
                .into_response();
        }
    };

    let mut decoded: Vec<(u32, Vec<[f32; 2]>)> = Vec::with_capacity(clips.len());
    for (id, bytes) in clips {
        match decode_wav_pcm16_stereo_any(&bytes) {
            Ok(pair) => decoded.push(pair),
            Err(err) => {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("decode {id}: {err}"),
                )
                    .into_response();
            }
        }
    }

    let sample_rate = decoded[0].0;
    if let Some((idx, (r, _))) = decoded.iter().enumerate().find(|(_, (r, _))| *r != sample_rate)
    {
        return (
            StatusCode::CONFLICT,
            format!(
                "clip {idx} has sample_rate {r} but script starts at {sample_rate}; \
                 re-render so every clip matches"
            ),
        )
            .into_response();
    }

    let gap_frames = ((SCRIPT_MIX_GAP_MS as u64 * sample_rate as u64) / 1000) as usize;
    let total_frames: usize = decoded.iter().map(|(_, s)| s.len()).sum::<usize>()
        + gap_frames * decoded.len().saturating_sub(1);
    let mut mixed: Vec<i32> = Vec::with_capacity(total_frames * 2);
    for (i, (_, pairs)) in decoded.iter().enumerate() {
        if i > 0 && gap_frames > 0 {
            mixed.extend(std::iter::repeat_n(0i32, gap_frames * 2));
        }
        for [l, r] in pairs.iter() {
            mixed.push(f32_to_i16_clipped(*l) as i32);
            mixed.push(f32_to_i16_clipped(*r) as i32);
        }
    }

    let flac = match tokio::task::spawn_blocking(move || encode_flac_stereo_i16(&mixed, sample_rate))
        .await
    {
        Ok(Ok(v)) => v,
        Ok(Err(err)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("flac encode: {err}"),
            )
                .into_response();
        }
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "flac encode task panicked",
            )
                .into_response();
        }
    };

    // Reuse the scene filename sanitizer so the same character rules apply —
    // it already handles empty / punctuation-only names by falling back to
    // "scene". A script-specific fallback would be nicer, but the sanitizer
    // is private to this file and the fallback isn't user-visible often.
    let filename = scene_mix_filename(if script_name.is_empty() {
        "script"
    } else {
        &script_name
    });
    info!(
        %script_id,
        clips = widget_ids.len(),
        bytes = flac.len(),
        filename = %filename,
        "script mix produced"
    );
    (
        [
            (header::CONTENT_TYPE, "audio/flac".to_string()),
            (header::CACHE_CONTROL, "no-store".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        Bytes::from(flac),
    )
        .into_response()
}

/// Decode a WAV in our dialect (16-bit little-endian PCM, mono or stereo)
/// into stereo pairs. Not a general WAV parser — just enough to round-trip
/// files produced by [`encode_wav_pcm16_stereo`] plus legacy mono widget
/// WAVs from before the profile-mix rewrite. Mono input is duplicated to
/// L=R so downstream code always sees stereo pairs. Returns (sample_rate,
/// pairs).
fn decode_wav_pcm16_stereo_any(bytes: &[u8]) -> Result<(u32, Vec<[f32; 2]>), String> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".into());
    }
    // Walk chunks after the RIFF header so we don't assume "fmt " lives at
    // exactly byte 12 (some encoders insert extra chunks or padding).
    let mut cursor = 12usize;
    let mut fmt: Option<(u16, u16, u32, u16)> = None; // (format, channels, sample_rate, bits)
    let mut data: Option<&[u8]> = None;
    while cursor + 8 <= bytes.len() {
        let id = &bytes[cursor..cursor + 4];
        let size = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        let body_start = cursor + 8;
        let body_end = body_start
            .checked_add(size)
            .ok_or_else(|| "chunk size overflow".to_string())?;
        if body_end > bytes.len() {
            return Err("truncated chunk".into());
        }
        match id {
            b"fmt " => {
                if size < 16 {
                    return Err("fmt chunk too small".into());
                }
                let b = &bytes[body_start..body_start + 16];
                fmt = Some((
                    u16::from_le_bytes([b[0], b[1]]),
                    u16::from_le_bytes([b[2], b[3]]),
                    u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
                    u16::from_le_bytes([b[14], b[15]]),
                ));
            }
            b"data" => {
                data = Some(&bytes[body_start..body_end]);
                break;
            }
            _ => {}
        }
        // Chunks are word-aligned — pad byte if size is odd.
        cursor = body_end + (size & 1);
    }
    let (format, channels, sample_rate, bits) = fmt.ok_or_else(|| "missing fmt chunk".to_string())?;
    let data = data.ok_or_else(|| "missing data chunk".to_string())?;
    if format != 1 || bits != 16 || !(channels == 1 || channels == 2) {
        return Err(format!(
            "unsupported format: pcm={} channels={} bits={}",
            format == 1,
            channels,
            bits
        ));
    }
    let bytes_per_frame = (channels as usize) * 2;
    let mut pairs = Vec::with_capacity(data.len() / bytes_per_frame);
    if channels == 1 {
        for c in data.chunks_exact(2) {
            let s = i16::from_le_bytes([c[0], c[1]]) as f32 / i16::MAX as f32;
            pairs.push([s, s]);
        }
    } else {
        for c in data.chunks_exact(4) {
            let l = i16::from_le_bytes([c[0], c[1]]) as f32 / i16::MAX as f32;
            let r = i16::from_le_bytes([c[2], c[3]]) as f32 / i16::MAX as f32;
            pairs.push([l, r]);
        }
    }
    Ok((sample_rate, pairs))
}

/// FLAC-encode a channel-interleaved i16 PCM buffer with 2 channels.
/// Samples are passed as `i32` (FLAC's interchange type) with layout
/// `[L0, R0, L1, R1, ...]`; only the low 16 bits carry data.
fn encode_flac_stereo_i16(samples_i32: &[i32], sample_rate: u32) -> Result<Vec<u8>, String> {
    use flacenc::bitsink::ByteSink;
    use flacenc::component::BitRepr;
    use flacenc::config::Encoder;
    use flacenc::error::Verify;
    use flacenc::source::MemSource;

    let config = Encoder::default()
        .into_verified()
        .map_err(|e| format!("bad flac config: {e:?}"))?;
    let source = MemSource::from_samples(samples_i32, 2, 16, sample_rate as usize);
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|e| format!("encode failed: {e:?}"))?;
    let mut sink = ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| format!("bitstream write failed: {e:?}"))?;
    Ok(sink.into_inner())
}

/// Build a `<safe-scene-name>_<utc-timestamp>.flac` filename. Sanitizes the
/// scene name to a safe subset so the Content-Disposition header stays
/// well-formed and the file lands on disk without escape issues.
fn scene_mix_filename(scene_name: &str) -> String {
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let mut safe: String = scene_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else if c.is_whitespace() {
                '-'
            } else {
                '_'
            }
        })
        .collect();
    // Trim runs of separators and edge separators for readability.
    while safe.contains("--") {
        safe = safe.replace("--", "-");
    }
    while safe.contains("__") {
        safe = safe.replace("__", "_");
    }
    let safe = safe.trim_matches(|c: char| c == '-' || c == '_').to_string();
    let safe = if safe.is_empty() { "scene".to_string() } else { safe };
    format!("{safe}_{ts}.flac")
}

// ---------------------------------------------------------------------------
// /images — server-side storage for character avatars + reference pictures.
//
// The client uploads raw bytes with a Content-Type header naming the image
// mime type; the server writes the file under data/images/{id} and records
// the mime on the sqlite row. Clients then reference the image via a plain
// `/images/{id}` URL as an <img> src.
// ---------------------------------------------------------------------------

async fn image_upload_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty body").into_response();
    }
    let mime = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !mime.starts_with("image/") {
        return (
            StatusCode::BAD_REQUEST,
            "Content-Type must be image/*".to_string(),
        )
            .into_response();
    }
    // Guard the mime column against odd headers with parameters we don't use.
    // e.g. `image/jpeg; charset=binary` → keep just the type/subtype.
    let mime_bare = mime.split(';').next().unwrap_or("image/octet-stream").trim();

    match state
        .store
        .create_image(mime_bare.to_string(), body.to_vec())
        .await
    {
        Ok(id) => {
            info!(%id, mime = %mime_bare, bytes = body.len(), "image stored");
            (
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "id": id,
                    "mime": mime_bare,
                    "size": body.len(),
                    "src": format!("/images/{id}"),
                })),
            )
                .into_response()
        }
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), "store: image create failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("store: {err:#}"),
            )
                .into_response()
        }
    }
}

async fn image_get_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let row = match state.store.get_image(id.clone()).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, format!("no image with id {id}"))
                .into_response();
        }
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), %id, "store: image get failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("store: {err:#}"),
            )
                .into_response();
        }
    };
    let path = state.store.image_path(&id);
    let bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (StatusCode::NOT_FOUND, format!("image file missing for {id}"))
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("image read: {e}"),
            )
                .into_response();
        }
    };
    // IDs are UUIDs and content is immutable at that id — safe to let the
    // browser cache aggressively.
    let _ = row.byte_size;
    (
        [
            (header::CONTENT_TYPE, row.mime),
            (
                header::CACHE_CONTROL,
                "public, max-age=31536000, immutable".to_string(),
            ),
        ],
        Bytes::from(bytes),
    )
        .into_response()
}

async fn image_delete_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    match state.store.delete_image(id.clone()).await {
        Ok(()) => {
            info!(%id, "image deleted");
            (
                StatusCode::OK,
                Json(ActionResponse {
                    ok: true,
                    error: None,
                }),
            )
                .into_response()
        }
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), %id, "store: image delete failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ActionResponse {
                    ok: false,
                    error: Some(format!("{err:#}")),
                }),
            )
                .into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// /voices — persistent compact voice-prompt files distilled from a user-
// supplied reference audio via the Qwen3 clone deploy's `/save_prompt`.
// Roundtrip: client POSTs the reference wav bytes (raw body) with metadata
// on the query string; server uploads to Gradio, calls save_prompt, GETs
// back the tiny voice-prompt file, stores it under `data/voices/{id}` and
// returns the id. Downstream synth reads the bytes and re-uploads to
// Gradio's /load_prompt_and_gen on every take (see SynthMode::Clone).
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct VoiceCreateQuery {
    /// Optional human-transcribed text of the reference wav. When absent
    /// (or empty), the server runs STT on the uploaded audio and uses
    /// that instead — the client shows the STT result in the description
    /// field for the user to review/edit afterward. When present, the
    /// client's text wins and STT is skipped.
    #[serde(default)]
    ref_txt: Option<String>,
    /// Toggle for Qwen3-TTS-Base's x-vector embedding path. `true` (default)
    /// uses the x-vector encoder; `false` skips it. Passed straight through
    /// to save_prompt.
    #[serde(default = "default_use_xvec")]
    use_xvec: bool,
    /// Original filename of the uploaded wav, used verbatim for the
    /// Gradio multipart upload. Defaults to `reference.wav` since Gradio
    /// only cares about the extension.
    #[serde(default = "default_ref_wav_name")]
    filename: String,
}
fn default_use_xvec() -> bool { true }
fn default_ref_wav_name() -> String { "reference.wav".to_string() }

async fn voice_create_handler(
    State(state): State<AppState>,
    Query(q): Query<VoiceCreateQuery>,
    body: Bytes,
) -> Response {
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty reference wav body").into_response();
    }

    // Client-supplied ref_txt wins. When empty, fall through to STT so
    // the user doesn't have to type the transcription first — they'll
    // see the STT result populated in the description field afterward
    // and can correct it there.
    let typed_ref_txt = q
        .ref_txt
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let ref_txt = match typed_ref_txt {
        Some(t) => t,
        None => {
            let Some(recog) = state.stt.clone() else {
                return (
                    StatusCode::BAD_REQUEST,
                    "ref_txt is required (STT is not loaded on this server)",
                )
                    .into_response();
            };
            // Decode + downmix to mono for the recognizer. The reused
            // decoder here accepts mono OR stereo PCM16 — the common
            // case for user-uploaded reference wavs — and rejects
            // fancier formats (float / 24-bit / mp3) with a clear error
            // that surfaces to the user as a 400.
            let (sample_rate, pairs) = match decode_wav_pcm16_stereo_any(&body) {
                Ok(v) => v,
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        format!(
                            "reference wav decode failed ({e}); \
                             upload PCM16 mono/stereo, or type the transcription manually"
                        ),
                    )
                        .into_response();
                }
            };
            if pairs.is_empty() {
                return (StatusCode::BAD_REQUEST, "reference wav has no samples").into_response();
            }
            let mono: Vec<f32> = pairs.iter().map(|p| (p[0] + p[1]) * 0.5).collect();
            match tokio::task::spawn_blocking(move || recog.transcribe(&mono, sample_rate)).await {
                Ok(Ok(t)) => {
                    let trimmed = t.trim().to_string();
                    if trimmed.is_empty() {
                        return (
                            StatusCode::BAD_REQUEST,
                            "STT produced an empty transcript (silent or non-speech audio?) — type the transcription manually",
                        )
                            .into_response();
                    }
                    info!(len = trimmed.len(), "STT transcript for reference wav");
                    trimmed
                }
                Ok(Err(err)) => {
                    tracing::warn!(err = %format!("{err:#}"), "STT failed on reference wav");
                    return (
                        StatusCode::BAD_GATEWAY,
                        format!("STT failed ({err:#}); type the transcription manually"),
                    )
                        .into_response();
                }
                Err(_) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "STT task panicked",
                    )
                        .into_response();
                }
            }
        }
    };

    // Fire the save_prompt command through the tts runner so the qwen3
    // backend (if active) can do the Gradio roundtrip. Any non-qwen3
    // backend replies with a clear "not supported" that we forward as a
    // 400 — the UI hides clone-mode when the backend isn't qwen3, so
    // reaching this branch means the client raced a settings change.
    let (reply_tx, reply_rx) = oneshot::channel();
    let cmd = tts::Command::SavePrompt(tts::SavePromptRequest {
        reference_wav: body.to_vec(),
        reference_wav_name: q.filename,
        ref_txt: ref_txt.clone(),
        use_xvec: q.use_xvec,
        reply: reply_tx,
    });
    if state.tts.send(cmd).await.is_err() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "tts runner has shut down",
        )
            .into_response();
    }
    let outcome = match reply_rx.await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return (StatusCode::BAD_GATEWAY, format!("save_prompt: {e}")).into_response();
        }
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "tts runner dropped the reply channel",
            )
                .into_response();
        }
    };
    match state
        .store
        .create_voice(
            outcome.voice_file_bytes.clone(),
            outcome.filename.clone(),
            Some(body.to_vec()),
        )
        .await
    {
        Ok(id) => {
            info!(
                %id,
                bytes = outcome.voice_file_bytes.len(),
                filename = %outcome.filename,
                "voice-prompt stored"
            );
            (
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "id": id,
                    "filename": outcome.filename,
                    "size": outcome.voice_file_bytes.len(),
                    // Echo back whatever text ended up feeding save_prompt
                    // (either the client's typed value, or the STT result)
                    // so the client can populate the description field.
                    "transcript": ref_txt,
                })),
            )
                .into_response()
        }
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), "store: voice create failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("store: {err:#}"),
            )
                .into_response()
        }
    }
}

/// Stream the original reference wav bytes for review playback in the
/// Characters UI. The saved voice-prompt file is the compact embedding
/// (~KB) that Qwen3 needs for synth; this endpoint is for humans who
/// want to hear what they actually uploaded. Response is
/// `audio/wav`; browsers can hand this straight to a `<audio>` tag.
async fn voice_reference_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let path = state.store.voice_reference_path(&id);
    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "audio/wav".to_string()),
                (
                    header::CACHE_CONTROL,
                    "public, max-age=31536000, immutable".to_string(),
                ),
            ],
            Bytes::from(bytes),
        )
            .into_response(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (
            StatusCode::NOT_FOUND,
            format!("no reference wav for voice {id}"),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("reference wav read: {e}"),
        )
            .into_response(),
    }
}

async fn voice_delete_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    match state.store.delete_voice(id.clone()).await {
        Ok(UpdateResult::Updated) => {
            info!(%id, "voice deleted");
            (StatusCode::OK, Json(ActionResponse { ok: true, error: None })).into_response()
        }
        Ok(UpdateResult::NotFound) => {
            (StatusCode::NOT_FOUND, format!("no voice {id}")).into_response()
        }
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), %id, "store: voice delete failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ActionResponse {
                    ok: false,
                    error: Some(format!("{err:#}")),
                }),
            )
                .into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// /samples — raw audio clips for SynthMode::Sample layers. Distinct from
// /voices: bytes are stored verbatim without any Qwen3 processing, so any
// codec symphonia handles (wav / flac / ogg / mp3) can back a Sample voice.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SampleCreateQuery {
    /// Original filename hint (used in the response so the client UI can
    /// show it back to the user). Optional; falls back to "sample.wav" so
    /// a curl POST without ?filename= still succeeds.
    #[serde(default = "default_sample_name")]
    filename: String,
}

fn default_sample_name() -> String { "sample.wav".to_string() }

async fn sample_create_handler(
    State(state): State<AppState>,
    Query(q): Query<SampleCreateQuery>,
    body: Bytes,
) -> Response {
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty sample body").into_response();
    }
    let size = body.len();
    match state.store.create_sample(body.to_vec(), q.filename.clone()).await {
        Ok(id) => {
            info!(%id, bytes = size, filename = %q.filename, "sample stored");
            (
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "id": id,
                    "filename": q.filename,
                    "size": size,
                })),
            )
                .into_response()
        }
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), "store: sample create failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("store: {err:#}"),
            )
                .into_response()
        }
    }
}

/// Stream the sample's raw bytes back for browser preview playback.
/// The MIME type is a best-effort guess based on the filename suffix;
/// unknown extensions fall through as `application/octet-stream` and
/// still play in most browsers via `<audio>` because the file magic
/// tells the decoder what it is.
async fn sample_audio_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    // Look up the filename first so we can pick a Content-Type; a missing
    // row → 404, matching /voices/:id/reference semantics.
    let filename = match state.store.get_sample_bytes(id.clone()).await {
        Ok(Some((_, name))) => name,
        Ok(None) => return (StatusCode::NOT_FOUND, format!("no sample {id}")).into_response(),
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), %id, "sample audio read failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("store: {err:#}")).into_response();
        }
    };
    let bytes = match tokio::fs::read(state.store.sample_path(&id)).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (StatusCode::NOT_FOUND, format!("no sample file for {id}")).into_response();
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("sample read: {e}")).into_response();
        }
    };
    let content_type = match filename.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "ogg" | "oga" => "audio/ogg",
        "mp3" => "audio/mpeg",
        "m4a" | "aac" => "audio/mp4",
        _ => "application/octet-stream",
    };
    (
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable".to_string()),
        ],
        Bytes::from(bytes),
    )
        .into_response()
}

async fn sample_delete_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    match state.store.delete_sample(id.clone()).await {
        Ok(UpdateResult::Updated) => {
            info!(%id, "sample deleted");
            (StatusCode::OK, Json(ActionResponse { ok: true, error: None })).into_response()
        }
        Ok(UpdateResult::NotFound) => {
            (StatusCode::NOT_FOUND, format!("no sample {id}")).into_response()
        }
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), %id, "store: sample delete failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ActionResponse {
                    ok: false,
                    error: Some(format!("{err:#}")),
                }),
            )
                .into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// /state — server-side persistence for the client's projects/characters/scenes
// blob. Treated as opaque JSON on the server; the shape is defined by
// `crates/rpg_vox/web/src/lib/scenes.svelte.js`. The client keeps only UI
// selection (current project/scene) in localStorage.
// ---------------------------------------------------------------------------

async fn state_get_handler(State(state): State<AppState>) -> Response {
    match state.store.get_app_state().await {
        Ok(Some(raw)) => (
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            raw,
        )
            .into_response(),
        // Empty state → return literal `null` so the client can distinguish
        // "server has nothing yet" from "server has an empty object".
        Ok(None) => (
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            "null".to_string(),
        )
            .into_response(),
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), "store: state get failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("store: {err:#}"),
            )
                .into_response()
        }
    }
}

async fn state_put_handler(
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    // Validate JSON shape before persisting so a garbled write doesn't
    // corrupt the store. Cheap for the sizes we handle (single-digit MB).
    let text = match std::str::from_utf8(&body) {
        Ok(s) => s,
        Err(_) => return (StatusCode::BAD_REQUEST, "body is not utf-8").into_response(),
    };
    if serde_json::from_str::<serde_json::Value>(text).is_err() {
        return (StatusCode::BAD_REQUEST, "body is not valid JSON").into_response();
    }
    match state.store.put_app_state(text.to_string()).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => {
            tracing::error!(err = %format!("{err:#}"), "store: state put failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("store: {err:#}"),
            )
                .into_response()
        }
    }
}

/// Clip an f32 sample to ±1.0 and convert to i16. Saturating instead of
/// wrapping since TTS samples occasionally sit right at ±1.0.
fn f32_to_i16_clipped(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

/// Return the `[start, end)` sample range containing all audio above a
/// silence threshold, with small pre/post rolls preserved so the attack
/// and decay of speech aren't chopped. Returns the full range when the
/// whole clip is below threshold — a silent recording stays silent rather
/// than becoming a zero-length clip.
fn trim_silence_range(pairs: &[[f32; 2]], sample_rate: u32) -> (usize, usize) {
    // ~-45 dBFS: above the noise floor of a quiet virtual pipewire sink,
    // low enough to catch a soft speech onset. If a hot mic pushes the
    // noise floor above this, the user gets a partial trim rather than
    // clipped audio — we prefer keeping too much to cutting real speech.
    const THRESHOLD: f32 = 0.0056;
    let pre_roll = (sample_rate as usize * 30) / 1000;   // 30 ms head
    let post_roll = (sample_rate as usize * 120) / 1000; // 120 ms tail

    let is_hot = |p: &[f32; 2]| p[0].abs().max(p[1].abs()) > THRESHOLD;
    let first = pairs.iter().position(is_hot);
    let last = pairs.iter().rposition(is_hot);
    match (first, last) {
        (Some(f), Some(l)) => {
            let start = f.saturating_sub(pre_roll);
            let end = (l + 1 + post_roll).min(pairs.len());
            (start, end)
        }
        _ => (0, pairs.len()),
    }
}

/// Encode a stereo f32 PCM buffer as a 16-bit little-endian WAV
/// (channel-interleaved: `[L0, R0, L1, R1, ...]`). Handrolled to avoid
/// pulling in `hound` for one call site.
fn encode_wav_pcm16_stereo(pairs: &[[f32; 2]], sample_rate: u32) -> Vec<u8> {
    let channels: u16 = 2;
    let bits: u16 = 16;
    let byte_rate = sample_rate * channels as u32 * (bits / 8) as u32;
    let block_align = channels * (bits / 8);
    let data_bytes: u32 = (pairs.len() * (channels as usize) * 2) as u32;
    let chunk_size: u32 = 36 + data_bytes;

    let mut out = Vec::with_capacity(44 + data_bytes as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&chunk_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());        // PCM subchunk size
    out.extend_from_slice(&1u16.to_le_bytes());         // PCM format
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());

    for [l, r] in pairs {
        out.extend_from_slice(&f32_to_i16_clipped(*l).to_le_bytes());
        out.extend_from_slice(&f32_to_i16_clipped(*r).to_le_bytes());
    }
    out
}
