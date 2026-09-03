//! Local HTTP API. Currently one endpoint:
//!
//!   POST /say  { "text": "..." }
//!
//! The text is forwarded to the TTS task via an mpsc channel. Requests return
//! as soon as the utterance is queued — audio playback happens asynchronously
//! through the PipeWire source.

use anyhow::{Context as _, Result};
use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::post, Json, Router};
use serde::Deserialize;
use tokio::sync::mpsc::Sender;
use tracing::info;

#[derive(Clone)]
struct AppState {
    say: Sender<String>,
}

#[derive(Debug, Deserialize)]
struct SayRequest {
    text: String,
}

pub async fn serve(bind: String, say: Sender<String>) -> Result<()> {
    let state = AppState { say };
    let app = Router::new()
        .route("/say", post(say_handler))
        .route("/healthz", axum::routing::get(|| async { "ok" }))
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

async fn say_handler(
    State(state): State<AppState>,
    Json(req): Json<SayRequest>,
) -> impl IntoResponse {
    let text = req.text.trim().to_string();
    if text.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty text").into_response();
    }
    match state.say.send(text).await {
        Ok(_) => (StatusCode::ACCEPTED, "queued").into_response(),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "tts task gone").into_response(),
    }
}
