//! Local HTTP API.
//!
//! Two synthesis paths sharing the same TTS backend:
//!
//!   POST /say                  { text, speaker?, language?, instruct? }
//!                              → { ok, frames } — pushes to the virtual
//!                              mic; used by Chat.
//!   POST /widgets              { text, speaker?, language?, instruct? }
//!                              → audio/wav — synthesize, persist a new
//!                              widget row + WAV in the store, return audio.
//!   PUT  /widgets/:id          { text, speaker?, language?, instruct? }
//!                              → audio/wav — re-synthesize, overwrite the
//!                              widget's row + WAV file.
//!   DELETE /widgets/:id                             → 200 — drop the DB
//!                              row and the WAV file (idempotent).
//!
//! All three synthesis paths go through the single-threaded TTS runner over
//! the same mpsc so backend access is serialized. `/widgets/*` additionally
//! reads/writes the sqlite + on-disk clip store from `store.rs`.

use anyhow::{Context as _, Result};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderName, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc::Sender, oneshot};
use tracing::info;

use std::sync::Arc;

use crate::chat;
use crate::pw_source::{GraphSnapshot, PwClient};
use crate::settings::{self, SettingsUpdate};
use crate::store::{Store, UpdateResult};
use crate::tts::{self, Command, SayRequest, SynthesizeRequest, VoiceOverride};
use crate::workflow::{self, Registry};

/// Built by build.rs (`pnpm run build` in crates/rpg_vox/web) into
/// crates/rpg_vox/dist-ui/. Baked into the binary at compile time.
#[derive(RustEmbed)]
#[folder = "dist-ui/"]
struct Ui;

#[derive(Clone)]
struct AppState {
    tts: Sender<Command>,
    pw: PwClient,
    settings: settings::Shared,
    registry: Arc<Registry>,
    chat: chat::Client,
    store: Store,
}

#[derive(Debug, Deserialize)]
struct SayBody {
    text: String,
    /// Per-request voice overrides. Only Qwen3 uses them; others ignore.
    /// Empty strings are coerced to `None` so a UI can send `""` freely.
    #[serde(default)]
    speaker: Option<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    instruct: Option<String>,
}

/// Coerce empty/whitespace-only strings to `None` so downstream defaults win.
fn trim_opt(s: Option<String>) -> Option<String> {
    s.and_then(|v| {
        let t = v.trim().to_string();
        (!t.is_empty()).then_some(t)
    })
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
) -> Result<()> {
    let state = AppState {
        tts,
        pw,
        settings,
        registry,
        chat,
        store,
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
        .route("/healthz", get(|| async { "ok" }))
        .route("/pw/graph", get(graph_handler))
        .route(
            "/pw/monitor",
            post(monitor_start_handler).delete(monitor_stop_handler),
        )
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
    let voice = VoiceOverride {
        speaker: trim_opt(body.speaker),
        language: trim_opt(body.language),
        instruct: trim_opt(body.instruct),
    };

    let (reply_tx, reply_rx) = oneshot::channel();
    if state
        .tts
        .send(Command::Say(SayRequest {
            text,
            voice,
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
                voice: VoiceOverride::default(),
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
    #[serde(default)]
    speaker: Option<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    instruct: Option<String>,
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

/// Push a synth request through the TTS runner, encode WAV, return it. The
/// caller decides what to do with the bytes (persist + respond, in the
/// widget handlers below).
async fn render_clip(
    state: &AppState,
    text: String,
    voice: VoiceOverride,
) -> Result<RenderedClip, (StatusCode, String)> {
    let (reply_tx, reply_rx) = oneshot::channel();
    if state
        .tts
        .send(Command::Synthesize(SynthesizeRequest {
            text,
            voice,
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
    let wav = encode_wav_pcm16(&outcome.samples, outcome.sample_rate);
    Ok(RenderedClip {
        wav,
        sample_rate: outcome.sample_rate,
        duration_ms,
    })
}

/// Parse+validate a widget body. Empty text is rejected; empty voice fields
/// are coerced to `None` so the backend falls back to its startup default.
/// Persisted `instruct` (used later by the store row) is returned alongside
/// the [`VoiceOverride`] the runner consumes.
fn normalize_widget_body(
    body: WidgetBody,
) -> Result<(String, VoiceOverride), (StatusCode, String)> {
    let text = body.text.trim().to_string();
    if text.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "empty text".into()));
    }
    let voice = VoiceOverride {
        speaker: trim_opt(body.speaker),
        language: trim_opt(body.language),
        instruct: trim_opt(body.instruct),
    };
    Ok((text, voice))
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
    let (text, voice) = match normalize_widget_body(body) {
        Ok(t) => t,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let persist_instruct = voice.instruct.clone();
    let clip = match render_clip(&state, text.clone(), voice).await {
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
    let (text, voice) = match normalize_widget_body(body) {
        Ok(t) => t,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let persist_instruct = voice.instruct.clone();
    let clip = match render_clip(&state, text.clone(), voice).await {
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

/// Encode a mono f32 PCM buffer as a 16-bit little-endian WAV. Handrolled to
/// avoid pulling in `hound` for one call site. Clips saturating instead of
/// wrapping since TTS samples occasionally sit right at ±1.0.
fn encode_wav_pcm16(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let channels: u16 = 1;
    let bits: u16 = 16;
    let byte_rate = sample_rate * channels as u32 * (bits / 8) as u32;
    let block_align = channels * (bits / 8);
    let data_bytes: u32 = (samples.len() * 2) as u32;
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

    for &s in samples {
        let clipped = s.clamp(-1.0, 1.0);
        let i = (clipped * i16::MAX as f32) as i16;
        out.extend_from_slice(&i.to_le_bytes());
    }
    out
}
