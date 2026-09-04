//! Local HTTP API. Currently one endpoint:
//!
//!   POST /say  { "text": "..." }
//!
//! The text is forwarded to the TTS task via an mpsc channel. Requests return
//! as soon as the utterance is queued — audio playback happens asynchronously
//! through the PipeWire source.

use anyhow::{Context as _, Result};
use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, header},
    response::{Html, IntoResponse},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc::Sender, oneshot};
use tracing::info;

use std::sync::Arc;

use crate::chat;
use crate::pw_source::{GraphSnapshot, PwClient};
use crate::settings::{self, SettingsUpdate};
use crate::tts::{self, SayRequest};
use crate::workflow::{self, Registry};

const INDEX_HTML: &str = include_str!("ui.html");

#[derive(Clone)]
struct AppState {
    say: Sender<SayRequest>,
    pw: PwClient,
    settings: settings::Shared,
    registry: Arc<Registry>,
    chat: chat::Client,
}

#[derive(Debug, Deserialize)]
struct SayBody {
    text: String,
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
    say: Sender<SayRequest>,
    pw: PwClient,
    settings: settings::Shared,
    registry: Arc<Registry>,
    chat: chat::Client,
) -> Result<()> {
    let state = AppState {
        say,
        pw,
        settings,
        registry,
        chat,
    };
    let app = Router::new()
        .route("/", get(index))
        .route("/say", post(say_handler))
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

async fn index() -> impl IntoResponse {
    ([(header::CACHE_CONTROL, "no-store")], Html(INDEX_HTML))
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

    let (reply_tx, reply_rx) = oneshot::channel();
    if state
        .say
        .send(SayRequest {
            text,
            reply: reply_tx,
        })
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
        speak_async(state.say.clone(), reply.clone());
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
fn speak_async(say: Sender<SayRequest>, text: String) {
    tokio::spawn(async move {
        let (tx, rx) = oneshot::channel();
        if say.send(SayRequest { text, reply: tx }).await.is_err() {
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
