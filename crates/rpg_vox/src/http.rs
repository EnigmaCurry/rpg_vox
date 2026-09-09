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
//!                              mic; used by Chat + Scenes.
//!   POST /widgets              { text, configs: [{...}, ...] }
//!                              → audio/wav (stereo) — synthesize, persist
//!                              a new widget row + WAV in the store,
//!                              return audio.
//!   PUT  /widgets/:id          { text, configs: [{...}, ...] }
//!                              → audio/wav — re-synthesize, overwrite the
//!                              widget's row + WAV file.
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
    extract::{DefaultBodyLimit, Path, State},
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
use crate::settings::{self, SettingsUpdate};
use crate::store::{Store, UpdateResult};
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
}

impl ConfigBody {
    fn into_voice_config(self) -> VoiceConfig {
        VoiceConfig {
            speaker: trim_opt(self.speaker),
            language: trim_opt(self.language),
            instruct: trim_opt(self.instruct),
            pitch_semitones: self.pitch_semitones.unwrap_or(0.0),
            time_ratio: self.time_ratio.unwrap_or(1.0),
            detune_cents: self.detune_cents.unwrap_or(0.0),
            pan: self.pan.unwrap_or(0.0),
            gain_db: self.gain_db.unwrap_or(0.0),
            delay_ms: self.delay_ms.unwrap_or(0.0),
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
                .delete(widget_delete_handler),
        )
        .route("/widgets/:id/say", post(widget_say_handler))
        .route("/playback/stop", post(playback_stop_handler))
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
        .route(
            "/chat",
            get(chat_history).post(chat_send).delete(chat_reset),
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

#[derive(Debug, Deserialize)]
struct ChatSendBody {
    text: String,
    /// If true (default), the assistant's reply is queued into the TTS path so
    /// the mic speaks it.
    #[serde(default = "default_speak")]
    speak: bool,
}

fn default_speak() -> bool {
    true
}

#[derive(Debug, Serialize)]
struct ChatSendResponse {
    ok: bool,
    reply: Option<String>,
    spoken: bool,
    error: Option<String>,
}

async fn chat_history(State(state): State<AppState>) -> impl IntoResponse {
    let h = state.chat.history_snapshot().await;
    (StatusCode::OK, Json(h)).into_response()
}

async fn chat_reset(State(state): State<AppState>) -> impl IntoResponse {
    state.chat.reset().await;
    (
        StatusCode::OK,
        Json(ActionResponse {
            ok: true,
            error: None,
        }),
    )
        .into_response()
}

async fn chat_send(
    State(state): State<AppState>,
    Json(body): Json<ChatSendBody>,
) -> impl IntoResponse {
    let text = body.text.trim().to_string();
    if text.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ChatSendResponse {
                ok: false,
                reply: None,
                spoken: false,
                error: Some("empty text".into()),
            }),
        )
            .into_response();
    }

    let reply = match state.chat.send(text).await {
        Ok(r) => r,
        Err(err) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(ChatSendResponse {
                    ok: false,
                    reply: None,
                    spoken: false,
                    error: Some(format!("{err:#}")),
                }),
            )
                .into_response();
        }
    };

    let spoken = if body.speak {
        speak_async(state.tts.clone(), reply.clone());
        true
    } else {
        false
    };

    (
        StatusCode::OK,
        Json(ChatSendResponse {
            ok: true,
            reply: Some(reply),
            spoken,
            error: None,
        }),
    )
        .into_response()
}

/// Fire-and-forget a TTS request; log any error so the chat HTTP response
/// isn't held up on audio generation.
fn speak_async(tts: Sender<Command>, text: String) {
    tokio::spawn(async move {
        let (tx, rx) = oneshot::channel();
        if tts
            .send(Command::Say(SayRequest {
                text,
                configs: vec![VoiceConfig::default()],
                reply: tx,
            }))
            .await
            .is_err()
        {
            tracing::warn!("tts channel closed; chat reply not spoken");
            return;
        }
        match rx.await {
            Ok(Ok(frames)) => tracing::debug!(frames, "chat reply queued to mic"),
            Ok(Err(err)) => tracing::warn!(err = %err, "chat reply tts failed"),
            Err(_) => tracing::warn!("tts dropped reply oneshot"),
        }
    });
}

// ---------------------------------------------------------------------------
// /widgets — persistent clip design.
//
// Each render (create or update) synthesizes fresh PCM, writes it to
// `data/clips/{id}.wav`, and mirrors the widget's text+instruct into the
// sqlite row. Delete drops both. See `store.rs` for the storage details.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct WidgetBody {
    text: String,
    /// Voice profile — see [`SayBody`] for shape and fallback semantics.
    #[serde(default)]
    configs: Vec<ConfigBody>,
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
async fn render_clip(
    state: &AppState,
    text: String,
    configs: Vec<VoiceConfig>,
) -> Result<RenderedClip, (StatusCode, String)> {
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
}

fn normalize_widget_body(body: WidgetBody) -> Result<NormalizedWidget, (StatusCode, String)> {
    let text = body.text.trim().to_string();
    if text.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "empty text".into()));
    }
    let configs = configs_or_default(body.configs);
    let persist_instruct = configs.first().and_then(|c| c.instruct.clone());
    Ok(NormalizedWidget {
        text,
        configs,
        persist_instruct,
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
    let NormalizedWidget { text, configs, persist_instruct } = match normalize_widget_body(body) {
        Ok(t) => t,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let clip = match render_clip(&state, text.clone(), configs).await {
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
    let NormalizedWidget { text, configs, persist_instruct } = match normalize_widget_body(body) {
        Ok(t) => t,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let clip = match render_clip(&state, text.clone(), configs).await {
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
    let mono_for_stt: Vec<f32> = pairs.iter().map(|p| (p[0] + p[1]) * 0.5).collect();

    let sample_rate = state.vox_sample_rate;
    let duration_ms = (pairs.len() as u64 * 1000) / sample_rate.max(1) as u64;

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
    let wav = encode_wav_pcm16_stereo(&pairs, sample_rate);

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

    info!(%id, %session_id, frames = pairs.len(), duration_ms, "recording saved as widget");
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
