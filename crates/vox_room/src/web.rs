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
//!
//! Events are filtered here: one addressed to a person (a card the AI
//! dealt them) is only written to that person's own sockets.
//!
//! The door is three requests. An invite link (`/join/<token>`) only
//! remembers the token in a short-lived cookie and sends the browser to
//! the room; `GET /api/door` says whose link it is (or who this browser
//! already is, which is all the room will answer about itself before the
//! cookie exists); and `POST /api/door/join` -- Join Audio -- trades the
//! link for the lasting cookie, which is the only thing the room asks
//! for afterwards. The link works once. `POST /api/door/name` is the
//! name they want to be called, which they may set before spending it.
//! `POST /api/door/logout` gives the cookie back: this browser is
//! forgotten, and since the link it came in on is spent, coming back
//! takes a new one.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::{header, HeaderMap, StatusCode, Uri},
    response::{AppendHeaders, IntoResponse, Redirect, Response},
    routing::{get, post},
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
use crate::screen::{Screen, Size};

/// This browser's own cookie, traded for an invite link at the door.
const COOKIE: &str = "vox_room";
/// The link just clicked, until Join Audio spends it. A dead token is
/// kept here too, so a browser coming back can be told it is dead.
const PENDING: &str = "vox_room_invite";
/// A browser is remembered for a year.
const SESSION_MAX_AGE: u32 = 31_536_000;
/// A year is far too long to hold a link that is about to be spent.
const PENDING_MAX_AGE: u32 = 86_400;

#[derive(RustEmbed)]
#[folder = "dist-ui/"]
struct Assets;

#[derive(Clone)]
struct App {
    room: Arc<Room>,
    screen: Arc<Screen>,
    next_id: Arc<AtomicU64>,
}

pub fn router(room: Arc<Room>, screen: Arc<Screen>) -> Router {
    let app = App {
        room,
        screen,
        // One counter for both sockets, so a tab's audio and its desktop
        // are never confused for each other in the logs.
        next_id: Arc::new(AtomicU64::new(1)),
    };
    Router::new()
        .route("/join/:token", get(join))
        .route("/api/door", get(door))
        .route("/api/door/join", post(door_join))
        .route("/api/door/name", post(door_name))
        .route("/api/door/language", post(door_language))
        .route("/api/door/logout", post(door_logout))
        .route("/api/games/:name/prompt", get(game_prompt))
        .route("/room.ws", get(ws))
        .route("/screen.ws", get(screen_ws))
        .route("/healthz", get(|| async { "ok" }))
        .fallback(assets)
        .with_state(app)
}

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

/// HTTPS, as far as the proxy in front of us is concerned: a cookie we
/// mark `Secure` on a plain-HTTP room would never come back.
fn secure(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|p| p == "https")
}

fn set_cookie(name: &str, value: &str, max_age: u32, headers: &HeaderMap) -> String {
    format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{}",
        if secure(headers) { "; Secure" } else { "" }
    )
}

/// The name the room knows this browser by, from its own cookie.
fn person(app: &App, headers: &HeaderMap) -> Option<String> {
    cookie(headers, COOKIE)
        .and_then(|id| app.room.person_for_session(&id))
        .map(|p| p.display)
}

/// An invite link. Nothing is spent here: the token is only remembered,
/// so the door can say whose link it is and let them look around before
/// Join Audio trades it in. Even a dead token is remembered, so the door
/// can say that it has expired.
async fn join(Path(t): Path<String>, headers: HeaderMap) -> Response {
    let pending = set_cookie(PENDING, &t, PENDING_MAX_AGE, &headers);
    ([(header::SET_COOKIE, pending)], Redirect::to("/")).into_response()
}

/// Who this browser is, or whose invite it is holding.
async fn door(State(app): State<App>, headers: HeaderMap) -> Response {
    // A link just clicked comes first: they navigated here to come in as
    // whoever it names, and the card says who that is before anything is
    // spent.
    if let Some(t) = cookie(&headers, PENDING) {
        if let Some(invite) = app.room.invite_at_door(&t) {
            return Json(json!({
                "state": "invited",
                "name": invite.name,
                "display": invite.display,
                // The door offers the language as well as the devices,
                // and this is the only place it can learn the list: the
                // socket that would otherwise carry it wants a session,
                // and the invite is not spent until Join Audio.
                "languages": app.room.config().languages,
            }))
            .into_response();
        }
        if person(&app, &headers).is_none() {
            // Used once already, or revoked, and this browser never kept
            // a cookie of its own.
            return (StatusCode::GONE, Json(json!({ "state": "expired" }))).into_response();
        }
    }
    match person(&app, &headers) {
        Some(display) => Json(json!({ "state": "in", "display": display })).into_response(),
        None => (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "state": "none", "error": "no invite" })),
        )
            .into_response(),
    }
}

/// Join Audio: trade the invite link for this browser's own cookie. The
/// link is spent here and nowhere else.
async fn door_join(State(app): State<App>, headers: HeaderMap) -> Response {
    let Some(t) = cookie(&headers, PENDING) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "no invite link" })),
        )
            .into_response();
    };
    let Some((id, invite)) = app.room.claim(&t) else {
        return (
            StatusCode::GONE,
            Json(json!({ "state": "expired", "error": "This link is expired." })),
        )
            .into_response();
    };
    info!(person = %invite.name, called = %invite.display, "came in on their invite link");
    // Two cookies of the same name, so they must be appended: a plain
    // array of headers would insert, and the second would replace the
    // first.
    (
        AppendHeaders([
            (
                header::SET_COOKIE,
                set_cookie(COOKIE, &id, SESSION_MAX_AGE, &headers),
            ),
            // Spent: nothing left to remember.
            (header::SET_COOKIE, set_cookie(PENDING, "", 0, &headers)),
        ]),
        Json(json!({ "state": "in", "display": invite.display })),
    )
        .into_response()
}

/// Log out: forget this browser, and hand back both cookies cleared.
/// Nobody else's is touched, and the invite stays spent -- coming back
/// takes a new link.
async fn door_logout(State(app): State<App>, headers: HeaderMap) -> Response {
    if let Some(id) = cookie(&headers, COOKIE) {
        if let Some(who) = app.room.person_for_session(&id) {
            info!(person = %who.name, "logged out of a browser");
        }
        app.room.log_out(&id);
    }
    (
        AppendHeaders([
            (header::SET_COOKIE, set_cookie(COOKIE, "", 0, &headers)),
            // A link left over from the way in goes with it, or the door
            // would offer to spend it again.
            (header::SET_COOKIE, set_cookie(PENDING, "", 0, &headers)),
        ]),
        Json(json!({ "state": "none" })),
    )
        .into_response()
}

#[derive(Deserialize)]
struct Rename {
    display: String,
}

/// What they want the room to call them, asked at the door before the
/// link is spent. Their invite's name doesn't move: it is the key the
/// cookie hangs on, and what `vox_room uninvite` names.
async fn door_name(
    State(app): State<App>,
    headers: HeaderMap,
    Json(body): Json<Rename>,
) -> Response {
    let Some(invite) = cookie(&headers, PENDING).and_then(|t| app.room.invite_at_door(&t)) else {
        return (
            StatusCode::GONE,
            Json(json!({ "error": "This link is expired." })),
        )
            .into_response();
    };
    match app.room.rename(&invite.name, &body.display) {
        Ok(display) => Json(json!({ "display": display })).into_response(),
        Err(why) => (StatusCode::CONFLICT, Json(json!({ "error": why }))).into_response(),
    }
}

#[derive(Deserialize)]
struct SetLanguage {
    language: String,
}

/// The language they speak and want to hear, chosen at the door. Unlike
/// the name it is not spent with the link, so it can be changed later by
/// a browser the room already knows -- which is why both the pending
/// invite and an established cookie are accepted here.
async fn door_language(
    State(app): State<App>,
    headers: HeaderMap,
    Json(body): Json<SetLanguage>,
) -> Response {
    let who = person(&app, &headers).or_else(|| {
        cookie(&headers, PENDING)
            .and_then(|t| app.room.invite_at_door(&t))
            .map(|i| i.display)
    });
    let Some(who) = who else {
        return (
            StatusCode::GONE,
            Json(json!({ "error": "This link is expired." })),
        )
            .into_response();
    };
    match app.room.set_language(&who, &body.language) {
        Ok(language) => Json(json!({ "language": language })).into_response(),
        Err(why) => (StatusCode::CONFLICT, Json(json!({ "error": why }))).into_response(),
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
        app.room.leave(&name, id);
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
    /// Typed into the room rather than said to it.
    Text {
        text: String,
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
    room.join(&name, id);
    room.mixer
        .add_listener(id, &name, &room.language_for(&name), audio_tx)?;
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
                    Ok(ev) if !ev.is_for(&name) => {}
                    Ok(ev) => out.send(Message::Text(ev.json.to_string())).await?,
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
                    Ok(Command::Mic { on }) => room.set_mic(&name, id, on),
                    Ok(Command::Mute { on }) => room.set_muted(&name, id, on),
                    Ok(Command::Ptt { on }) => room.set_ptt(&name, id, on),
                    Ok(Command::Strip {
                        name: strip,
                        gain,
                        mute,
                    }) => room.set_strip(&strip, gain, mute),
                    Ok(Command::Mode { mode }) => room.set_mode(&mode),
                    // Only the sender hears about a message the room
                    // wouldn't take: it is their typing, not news.
                    Ok(Command::Text { text }) => {
                        if let Err(why) = room.typed(&name, &text) {
                            room.tell(&name, &why);
                        }
                    }
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

// -- the desktop socket ---------------------------------------------------

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Share {
    Publish {
        codec: String,
        thumb: Size,
        full: Size,
    },
    Unpublish,
    /// Open one desktop full size, or `null` to close what was open.
    /// This tab's own business: nobody else's screen changes.
    Watch {
        id: Option<u16>,
    },
}

/// The same cookie as the room's socket. Sharing a desktop is for
/// whoever is already in.
async fn screen_ws(
    State(app): State<App>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let Some(name) = person(&app, &headers) else {
        return (StatusCode::UNAUTHORIZED, "no invite").into_response();
    };
    upgrade.on_upgrade(move |socket| async move {
        let id = app.next_id.fetch_add(1, Ordering::Relaxed);
        if let Err(e) = sharing(app.screen.clone(), socket, id, &name).await {
            debug!(id, "screen socket ended: {e:#}");
        }
        app.screen.remove_client(id);
    })
}

async fn sharing(
    screen: Arc<Screen>,
    socket: WebSocket,
    id: u64,
    person: &str,
) -> anyhow::Result<()> {
    let (mut frames, mut ctl) = screen.add_client(id, person);
    let (mut out, mut inp) = socket.split();

    let send = async {
        loop {
            tokio::select! {
                // Frames and control messages are separate queues: a
                // backed-up picture must not delay (or lose) the news
                // that somebody stopped sharing.
                frame = frames.recv() => match frame {
                    Some(f) => out.send(Message::Binary(f)).await?,
                    None => return anyhow::Ok(()),
                },
                text = ctl.recv() => match text {
                    Some(t) => out.send(Message::Text(t)).await?,
                    None => return Ok(()),
                },
            }
        }
    };

    let recv = async {
        while let Some(msg) = inp.next().await {
            match msg? {
                Message::Binary(frame) => screen.frame(id, frame),
                Message::Text(text) => match serde_json::from_str::<Share>(&text) {
                    Ok(Share::Publish { codec, thumb, full }) => {
                        screen.publish(id, codec, thumb, full)
                    }
                    Ok(Share::Unpublish) => screen.unpublish(id),
                    Ok(Share::Watch { id: stream }) => screen.watch(id, stream),
                    Err(e) => debug!(id, "bad screen command {text:?}: {e}"),
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
