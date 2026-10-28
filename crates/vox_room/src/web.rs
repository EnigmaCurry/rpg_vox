//! HTTP: the embedded web app, invite links, and the room's WebSocket.
//!
//! `/room.ws` carries everything for one tab. Binary messages from the
//! browser are 20 ms Opus frames of its mic; binary messages to it are
//! 20 ms Opus frames of its own mix. Text messages are JSON: room events
//! out (see [`crate::room`]), and commands in:
//!
//! ```json
//! {"type": "mic", "on": true}
//! {"type": "mute", "on": true}
//! {"type": "ptt", "on": true}
//! {"type": "strip", "name": "Alex", "gain": 0.8, "mute": false}
//! {"type": "mode", "mode": "always"}
//! {"type": "stop"}
//! {"type": "new_conversation"}
//! {"type": "enter_game", "name": "Dragon Heist"}
//! {"type": "lobby"}
//! {"type": "delete_game", "name": "Dragon Heist"}
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::{header, HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Redirect, Response},
    routing::get,
    Json, Router,
};
use futures_util::{SinkExt as _, StreamExt as _};
use rust_embed::RustEmbed;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

use crate::mixer::{LISTENER_QUEUE, RATE};
use crate::room::Room;

const COOKIE: &str = "vox_room";

#[derive(RustEmbed)]
#[folder = "dist-ui/"]
struct Assets;

#[derive(Clone)]
struct App {
    room: Arc<Room>,
    next_id: Arc<AtomicU64>,
}

pub fn router(room: Arc<Room>) -> Router {
    let app = App {
        room,
        next_id: Arc::new(AtomicU64::new(1)),
    };
    Router::new()
        .route("/join/:token", get(join))
        .route("/api/me", get(me))
        .route("/api/games/:name/prompt", get(game_prompt))
        .route("/room.ws", get(ws))
        .route("/healthz", get(|| async { "ok" }))
        .fallback(assets)
        .with_state(app)
}

fn token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE)
        .map(|(_, v)| v.to_string())
}

fn person(app: &App, headers: &HeaderMap) -> Option<String> {
    token(headers).and_then(|t| app.room.person_for_token(&t))
}

/// An invite link: remember the token and go to the room.
async fn join(State(app): State<App>, Path(t): Path<String>, headers: HeaderMap) -> Response {
    if app.room.person_for_token(&t).is_none() {
        return (
            StatusCode::FORBIDDEN,
            "This invite link isn't valid any more.",
        )
            .into_response();
    }
    let secure = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|p| p == "https");
    let cookie = format!(
        "{COOKIE}={t}; Path=/; HttpOnly; SameSite=Lax; Max-Age=31536000{}",
        if secure { "; Secure" } else { "" }
    );
    ([(header::SET_COOKIE, cookie)], Redirect::to("/")).into_response()
}

async fn me(State(app): State<App>, headers: HeaderMap) -> Response {
    match person(&app, &headers) {
        Some(name) => Json(json!({ "name": name })).into_response(),
        None => (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "no invite" })),
        )
            .into_response(),
    }
}

/// A game's hidden prompt, for one person who asked to peek. It is never
/// broadcast to the room.
async fn game_prompt(
    State(app): State<App>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(who) = person(&app, &headers) else {
        return (StatusCode::UNAUTHORIZED, "no invite").into_response();
    };
    match app.room.game_with_prompt(&name) {
        Some((game, prompt)) => {
            info!(person = %who, game = %game.name, "peeked at a game's prompt");
            Json(json!({ "name": game.name, "summary": game.summary, "prompt": prompt }))
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "no such game").into_response(),
    }
}

async fn assets(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    // Unknown paths get the app, which has only the one page.
    match Assets::get(path).or_else(|| Assets::get("index.html")) {
        Some(file) => {
            let mime = file.metadata.mimetype().to_string();
            ([(header::CONTENT_TYPE, mime)], file.data).into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

async fn ws(State(app): State<App>, headers: HeaderMap, upgrade: WebSocketUpgrade) -> Response {
    let Some(name) = person(&app, &headers) else {
        return (StatusCode::UNAUTHORIZED, "no invite").into_response();
    };
    upgrade.on_upgrade(move |socket| async move {
        let id = app.next_id.fetch_add(1, Ordering::Relaxed);
        if let Err(e) = client(app.room.clone(), socket, id, name.clone()).await {
            debug!(id, "client ended: {e:#}");
        }
        app.room.mixer.remove_listener(id);
        app.room.leave(&name);
    })
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Command {
    Mic {
        on: bool,
    },
    Mute {
        on: bool,
    },
    Ptt {
        on: bool,
    },
    Strip {
        name: String,
        gain: Option<f32>,
        mute: Option<bool>,
    },
    Mode {
        mode: String,
    },
    Stop,
    NewConversation,
    EnterGame {
        name: String,
    },
    Lobby,
    DeleteGame {
        name: String,
    },
}

async fn client(room: Arc<Room>, socket: WebSocket, id: u64, name: String) -> anyhow::Result<()> {
    let (audio_tx, mut audio_rx) = mpsc::channel::<Vec<u8>>(LISTENER_QUEUE);
    let mut events = room.subscribe();
    room.join(&name);
    room.mixer.add_listener(id, &name, audio_tx)?;
    let (mut out, mut inp) = socket.split();
    out.send(Message::Text(room.hello(&name).to_string()))
        .await?;
    info!(id, person = %name, "client connected");

    let send = async {
        loop {
            tokio::select! {
                frame = audio_rx.recv() => match frame {
                    Some(f) => out.send(Message::Binary(f)).await?,
                    None => return anyhow::Ok(()),
                },
                ev = events.recv() => match ev {
                    Ok(text) => out.send(Message::Text(text.to_string())).await?,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // Too far behind to patch up: have the tab start over.
                        warn!(id, missed = n, "client lagged; resending the room");
                        out.send(Message::Text(room.hello(&name).to_string())).await?;
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
            }
        }
    };

    let recv = async {
        let mut decoder = opus::Decoder::new(RATE, opus::Channels::Mono)?;
        // 120 ms, the longest Opus frame.
        let mut pcm = vec![0.0f32; RATE as usize / 1000 * 120];
        while let Some(msg) = inp.next().await {
            match msg? {
                Message::Binary(frame) => match decoder.decode_float(&frame, &mut pcm, false) {
                    Ok(n) => room.mic_audio(&name, &pcm[..n]),
                    Err(e) => debug!(id, "bad opus frame: {e}"),
                },
                Message::Text(text) => match serde_json::from_str::<Command>(&text) {
                    Ok(Command::Mic { on }) => room.set_mic(&name, on),
                    Ok(Command::Mute { on }) => room.set_muted(&name, on),
                    Ok(Command::Ptt { on }) => room.set_ptt(&name, on),
                    Ok(Command::Strip {
                        name: strip,
                        gain,
                        mute,
                    }) => room.set_strip(&strip, gain, mute),
                    Ok(Command::Mode { mode }) => room.set_mode(&mode),
                    Ok(Command::Stop) => room.stop_agent(),
                    Ok(Command::NewConversation) => room.new_conversation(),
                    Ok(Command::EnterGame { name }) => room.enter_game(&name),
                    Ok(Command::Lobby) => room.lobby(),
                    Ok(Command::DeleteGame { name }) => room.delete_game(&name),
                    Err(e) => debug!(id, "bad command {text:?}: {e}"),
                },
                Message::Close(_) => break,
                _ => {}
            }
        }
        anyhow::Ok(())
    };

    tokio::select! {
        r = send => r,
        r = recv => r,
    }
}
