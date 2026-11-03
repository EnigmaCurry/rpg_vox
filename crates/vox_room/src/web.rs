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
//! {"type": "interpreter", "gain": 0.8, "mute": false}
//! {"type": "mode", "bot": "Vox", "mode": "always"}
//! {"type": "stops", "bot": "Vox", "stops": "finished"}
//! {"type": "name_conversation", "conversation": 7, "title": "The heist"}
//! {"type": "stop"}
//! {"type": "new_conversation"}
//! {"type": "enter_game", "name": "Dragon Heist"}
//! {"type": "lobby"}
//! {"type": "delete_game", "name": "Dragon Heist"}
//! {"type": "enter_room", "id": "@Ryan"}
//! ```
//!
//! Events are filtered here: one addressed to a person (a card the AI
//! dealt them) is only written to that person's own sockets.
//!
//! **One socket, several rooms.** A server holds more than one room (see
//! [`crate::rooms`]), and `enter_room` moves this tab from one to
//! another without the connection going anywhere: the mic frames keep
//! arriving and the mix keeps going out on the same channel, and what
//! changes is which room's mixer the channel is plugged into, which
//! room's roster the tab is on, and whose conversation its events come
//! from. The tab is then sent a whole new `hello`, because everything
//! it was showing belonged somewhere else.
//!
//! Every tab is also sent a `rooms` event -- the rooms it may go to and
//! who is in each -- when it connects and whenever anybody arrives,
//! leaves or moves. It is worked out per person, since a room of
//! somebody's own is nobody else's business.
//!
//! **The door hands this browser nothing until Join Audio.** An invite
//! link (`/join/<token>`) is served the app and nothing else: no
//! cookie, no redirect, nothing written down. The token stays in the
//! address bar, where whoever followed the link put it, and the page
//! holds it in memory for as long as it is open -- so a browser that
//! has looked at a link and not used it is a browser the room knows
//! nothing about, and closing the tab is all it takes to be rid of it.
//!
//! The door is three requests, and the page sends the token with each:
//! `POST /api/door` says whose link it is (or who this browser already
//! is, which is all the room will answer about itself before there is a
//! cookie); `POST /api/door/name` is the name they want to be called,
//! which they may set before spending the link; and `POST
//! /api/door/join` -- Join Audio -- spends it for the lasting cookie,
//! which is the only thing the room asks for afterwards, and the first
//! thing this browser is given to keep. The link works once.
//!
//! `POST /api/door/logout` gives the cookie back: this browser is
//! forgotten, and since the link it came in on is spent, coming back
//! takes a new one.
//!
//! A link the *page* is holding only speaks for it while the browser
//! has no cookie of its own. One browser is one person, so a browser
//! already in the room is told it is carrying somebody else's link
//! rather than being handed their name, and logging out is what gives
//! the room over to it. None of these answers may be cached: every one
//! of them is about the one browser that asked.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        DefaultBodyLimit, Path, Query, Request, State,
    },
    http::{header, HeaderMap, HeaderValue, StatusCode, Uri},
    middleware::{from_fn, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use futures_util::{SinkExt as _, StreamExt as _};
use rust_embed::RustEmbed;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, watch};
use tracing::{debug, info, warn};

use crate::mixer::{LISTENER_QUEUE, RATE};
use crate::room::Room;
use crate::rooms::Rooms;
use crate::screen::{Screen, Size};
use crate::store::Invited;

/// This browser's own cookie, traded for an invite link at the door.
/// The only thing the room ever puts in a browser, and Join Audio is
/// the only thing that puts it there.
const COOKIE: &str = "vox_room";
/// A browser is remembered for a year.
const SESSION_MAX_AGE: u32 = 31_536_000;

/// Where the app's hashed files live. `index.html` is the only thing
/// that names them, and the only name under here that never changes is
/// the directory itself.
const ASSETS: &str = "assets/";
/// A file named after a hash of its own contents can be kept for as
/// long as the browser likes: a change to it is a new name, never a
/// different answer at the old one.
const ASSET_CACHE: &str = "public, max-age=31536000, immutable";
/// A face, which is asked for once per version and then never again:
/// the version it was set at is in the URL, so a new one is a new
/// address. Private, because the house is private -- the bytes are the
/// same for everybody who may see them, and nobody else may.
const AVATAR_CACHE: &str = "private, max-age=31536000, immutable";
/// `index.html` is stored but asked about every time. It is what says
/// which hashed files the app is made of *now*, and a browser holding
/// yesterday's index asks this binary for files it no longer has.
const INDEX_CACHE: &str = "no-cache";

/// How much of a dropped picture the server will read at all, which is
/// a little over what it will keep: the room's own answer -- the size
/// in kilobytes and what the limit is -- is worth more than a 413 from
/// the layer, and that needs the bytes to have arrived.
const PICTURE_BODY_MAX: usize = crate::room::AVATAR_BYTES_MAX + 64 * 1024;

/// `?name=` on a dropped picture: the file it came in as, which is the
/// caption on its card. Not trusted for anything else -- it is written
/// by whoever dropped it.
#[derive(Deserialize)]
struct PictureName {
    #[serde(default)]
    name: String,
}

/// `?v=` on a face: which one of somebody's to serve.
#[derive(Deserialize)]
struct AvatarQuery {
    v: Option<u64>,
}

/// Which past face to put back on.
#[derive(Deserialize)]
struct WearAvatar {
    at: u64,
}

#[derive(RustEmbed)]
#[folder = "dist-ui/"]
struct Assets;

#[derive(Clone)]
struct App {
    rooms: Arc<Rooms>,
    screen: Arc<Screen>,
    next_id: Arc<AtomicU64>,
}

impl App {
    /// A room to ask about things that are the server's and not any one
    /// room's: who is invited, what games are saved. Every room shares
    /// the one database, so the first will do.
    fn any(&self) -> Arc<Room> {
        self.rooms.home()
    }
}

/// Nothing below this is the same answer twice: every one of these
/// endpoints says something about the one browser that asked -- who it
/// is, whose link its page is holding, what the owner may see.
///
/// A cache is allowed to reuse a response it was told nothing about,
/// and one of them did: a private window held `GET /api/door`'s answer
/// from an earlier test in memory and greeted the next browsing session
/// with somebody else's name, on a fresh invite link that had set the
/// right cookie. `Vary` goes with it, so that a cache between us and
/// the browser -- the proxy out front today, a CDN tomorrow -- can
/// never hand one person's answer to another either.
async fn private_answer(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache, must-revalidate"),
    );
    h.insert(header::VARY, HeaderValue::from_static("Cookie"));
    res
}

pub fn router(rooms: Arc<Rooms>, screen: Arc<Screen>) -> Router {
    let app = App {
        rooms,
        screen,
        // One counter for both sockets, so a tab's audio and its desktop
        // are never confused for each other in the logs.
        next_id: Arc::new(AtomicU64::new(1)),
    };
    Router::new()
        // `/join/<token>` is not here: an invite link is served the app
        // like any other address the app knows and the server doesn't,
        // because there is nothing for the server to do with it until
        // the page sends the token back.
        .route("/api/door", post(door))
        .route("/api/door/join", post(door_join))
        .route("/api/door/name", post(door_name))
        .route("/api/door/language", post(door_language))
        .route("/api/door/logout", post(door_logout))
        // The house's people, which is the owner's business and
        // nobody else's. Every one of these says so for itself: an
        // endpoint that trusts the page not to have been asked is an
        // endpoint anybody can ask with curl.
        .route("/api/people", get(people).post(invite_person))
        .route("/api/people/:name", delete(uninvite_person))
        .route("/api/people/:name/link", post(relink_person))
        .route("/api/people/:name/rooms", post(set_room))
        .route("/api/bots/:name/rooms", post(set_bot_room))
        // The house's rooms, which are the owner's to make and unmake.
        .route("/api/house/rooms", post(make_room))
        .route(
            "/api/house/rooms/:id",
            post(rename_room).delete(delete_room),
        )
        .route("/api/games/:name/prompt", get(game_prompt))
        .route("/api/profile/:who", get(profile))
        // A picture dropped into the room. The one endpoint here that
        // takes bytes rather than JSON, and the only one with a limit
        // of its own: what it will keep is smaller than what axum
        // would otherwise refuse, and a browser that sends too much
        // should be told which of the two it ran into.
        .route(
            "/api/rooms/:room/picture",
            post(drop_picture).route_layer(DefaultBodyLimit::max(PICTURE_BODY_MAX)),
        )
        .route("/api/rooms/:room/history", get(history))
        .route("/api/rooms/:room/history/:id", get(read_conversation))
        .route(
            "/api/rooms/:room/history/:id/delete",
            post(forget_conversation),
        )
        // Everything above answers one browser about itself, so none
        // of it may be cached. The sockets, the health check, the
        // faces and the app's own files below are the same for
        // everybody -- a face at a given version is the same bytes to
        // whoever may see it, which is what lets it be cached at all.
        .layer(from_fn(private_answer))
        .route(
            "/api/avatar/:who",
            get(avatar).post(wear_avatar).delete(forget_avatar),
        )
        .route("/api/avatar/:who/all", get(faces))
        .route("/api/picture/:id", get(picture))
        .route("/api/sample/:id", get(sample))
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

/// Who this browser is, from its own cookie: the name on their invite
/// (which never changes, and is what a room of their own is theirs by)
/// and the name the rooms call them.
fn invited(app: &App, headers: &HeaderMap) -> Option<Invited> {
    cookie(headers, COOKIE).and_then(|id| app.any().person_for_session(&id))
}

/// The name the rooms know this browser by.
fn person(app: &App, headers: &HeaderMap) -> Option<String> {
    invited(app, headers).map(|p| p.display)
}

/// What the page at the door is holding: the token out of the address
/// bar, when it was opened on an invite link.
///
/// In the body rather than the path or the query, because a body is the
/// one part of the exchange an access log doesn't keep. The address bar
/// is the only copy of it there is, which is the point.
#[derive(Deserialize)]
struct AtTheDoor {
    #[serde(default)]
    token: Option<String>,
}

/// Who this browser is, or whose invite its page is holding.
async fn door(State(app): State<App>, headers: HeaderMap, Json(at): Json<AtTheDoor>) -> Response {
    let mine = invited(&app, &headers);
    // The link this page was opened on comes first: they followed it to
    // come in as whoever it names, and the card says who that is before
    // anything is spent.
    //
    // First only over a browser with no name of its own, though.
    // Somebody else's link is said plainly instead and left unspent:
    // one browser is one person, and the way to use it is to log out.
    if let Some(t) = &at.token {
        match app.any().invite_at_door(t) {
            Some(invite) if mine.as_ref().is_none_or(|me| me.name == invite.name) => {
                return Json(json!({
                    "state": "invited",
                    "name": invite.name,
                    "display": invite.display,
                    // The door offers the language as well as the devices,
                    // and this is the only place it can learn the list: the
                    // socket that would otherwise carry it wants a session,
                    // and the invite is not spent until Join Audio.
                    "languages": app.any().config().languages,
                }))
                .into_response();
            }
            Some(invite) => return in_already(&app, &mine.unwrap(), Some(&invite.name)),
            None if mine.is_none() => {
                // Used once already, or revoked, and this browser never
                // kept a cookie of its own.
                return (StatusCode::GONE, Json(json!({ "state": "expired" }))).into_response();
            }
            // A link of their own that they have already spent, opened
            // again: they are in, and that is what the door says.
            None => {}
        }
    }
    match &mine {
        Some(p) => in_already(&app, p, None),
        None => (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "state": "none", "error": "no invite" })),
        )
            .into_response(),
    }
}

/// What the door says to a browser the room already knows. `holding` is
/// the name on somebody else's unspent link that this browser is
/// carrying, when it is carrying one: the page says so rather than the
/// room acting on it, because logging out is the only thing that can.
fn in_already(app: &App, p: &Invited, holding: Option<&str>) -> Response {
    Json(json!({
        "state": "in",
        // Both names: the one on their invite, which whoever runs the
        // room chose and nobody can change, and the one the room calls
        // them, which is theirs. The settings page says which is which,
        // and it can't without being told.
        "name": p.name,
        "display": p.display,
        // The door is a page with no socket behind it for somebody
        // the room already knows, exactly as it is for an invite, so
        // this is the only place either of them can learn what the
        // room offers. Without it a returning browser was shown no
        // Language box at all until it had joined once and had the
        // list from a socket it no longer has.
        "languages": app.any().config().languages,
        // And what they chose last time, which the room has and a
        // fresh tab does not: it beats this browser's own guess.
        "language": p.language,
        // Whether this browser owns the house. The page uses it to
        // decide what to show; every endpoint that matters asks
        // again for itself.
        "owner": p.owner,
        "holding": holding,
    }))
    .into_response()
}

// -- the house's people, for the owner ---------------------------------
//
// The owner invites everybody else, hands out the rooms, and can take
// any of it back. Everything below asks who this browser is first and
// answers 404 to anybody who isn't them: not 403, because whether a
// room has a people list is itself none of their business.

/// This browser, if it belongs to the owner.
fn owner(app: &App, headers: &HeaderMap) -> Option<Invited> {
    invited(app, headers).filter(|p| p.owner)
}

/// What anybody but the owner is told about any of this.
fn not_for_them() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "no such thing" })),
    )
        .into_response()
}

fn said(err: anyhow::Error) -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({ "error": format!("{err:#}") })),
    )
        .into_response()
}

/// Everybody invited, what they may do and where they may go.
async fn people(State(app): State<App>, headers: HeaderMap) -> Response {
    let Some(me) = owner(&app, &headers) else {
        return not_for_them();
    };
    let everyone = match app.rooms.everyone() {
        Ok(p) => p,
        Err(err) => return said(err),
    };
    let rooms = app.rooms.shared_rooms();
    let mut out = Vec::new();
    for p in everyone {
        let grants = app.rooms.grants_of(&p.name).unwrap_or_default();
        out.push(json!({
            "name": p.name,
            "display": p.display,
            "owner": p.owner,
            "language": p.language,
            // The link while it is unspent, whole, because the page's
            // one job with it is to be copied out of. Once it is spent
            // there is nothing to show: a used link opens nothing.
            "link": p.link.map(|t| format!("/join/{t}")),
            "rooms": grants,
        }));
    }
    // The AIs are given rooms the same way people are, and the first
    // one is in all of them the way the owner is: ticked and fixed.
    let bots: Vec<Value> = app
        .rooms
        .bots()
        .into_iter()
        .enumerate()
        .map(|(n, name)| {
            let rooms = app.rooms.bot_rooms(&name).unwrap_or_default();
            json!({
                "name": name,
                "first": n == 0,
                "rooms": rooms,
                "own": rooms.iter().any(|r| r == Rooms::OWN),
            })
        })
        .collect();
    Json(json!({
        "you": me.name,
        // The lobby first, as everywhere else. The page says that one
        // is everybody's rather than offering it to be given.
        "allRooms": rooms.iter().map(|(id, title)| json!({ "id": id, "title": title }))
            .collect::<Vec<_>>(),
        "lobby": app.rooms.lobby(),
        "people": out,
        "bots": bots,
        // What a room of somebody's own is called in all of this: one
        // column, because it is one answer for every one of them.
        "own": Rooms::OWN,
    }))
    .into_response()
}

/// The owner putting one AI in a room, or taking it out.
async fn set_bot_room(
    State(app): State<App>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(body): Json<RoomFor>,
) -> Response {
    if owner(&app, &headers).is_none() {
        return not_for_them();
    }
    match app.rooms.set_bot_room(&name, &body.room, body.allowed) {
        Ok(_) => {
            info!(bot = %name, room = %body.room, allowed = body.allowed, "the owner said which rooms it is in");
            app.rooms.announce();
            Json(json!({ "rooms": app.rooms.bot_rooms(&name).unwrap_or_default() })).into_response()
        }
        Err(err) => said(err),
    }
}

#[derive(Deserialize)]
struct NewPerson {
    name: String,
}

/// Invite somebody, and hand their link back to be copied.
async fn invite_person(
    State(app): State<App>,
    headers: HeaderMap,
    Json(body): Json<NewPerson>,
) -> Response {
    if owner(&app, &headers).is_none() {
        return not_for_them();
    }
    let name = body.name.trim();
    if name.is_empty() || name.starts_with('@') {
        return said(anyhow::anyhow!("give the person a name"));
    }
    if name.chars().count() > crate::room::NAME_MAX {
        return said(anyhow::anyhow!(
            "names are at most {} characters",
            crate::room::NAME_MAX
        ));
    }
    match app.rooms.invite(name) {
        Ok(token) => {
            info!(person = %name, "the owner invited somebody");
            app.rooms.catch_up();
            Json(json!({ "name": name, "link": format!("/join/{token}") })).into_response()
        }
        Err(err) => said(err),
    }
}

/// A fresh link for somebody already invited.
async fn relink_person(
    State(app): State<App>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    if owner(&app, &headers).is_none() {
        return not_for_them();
    }
    match app.rooms.relink(&name) {
        Ok(token) => {
            info!(person = %name, "the owner made them a new link");
            Json(json!({ "name": name, "link": format!("/join/{token}") })).into_response()
        }
        Err(err) => said(err),
    }
}

/// Remove somebody: their link, their browsers and their rooms.
async fn uninvite_person(
    State(app): State<App>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(me) = owner(&app, &headers) else {
        return not_for_them();
    };
    if me.name == name {
        // Removing yourself from here would leave a house nobody can
        // manage and no way back into it from a browser. The command
        // line is where an owner is changed, on purpose.
        return said(anyhow::anyhow!(
            "you are the owner; `vox_room uninvite {name}` at the server if you mean to go"
        ));
    }
    match app.rooms.uninvite(&name) {
        Ok(true) => {
            info!(person = %name, "the owner removed somebody");
            app.rooms.catch_up();
            Json(json!({ "gone": name })).into_response()
        }
        Ok(false) => said(anyhow::anyhow!("{name} isn't invited")),
        Err(err) => said(err),
    }
}

#[derive(Deserialize)]
struct RoomFor {
    room: String,
    allowed: bool,
}

/// Give somebody a room, or take it away.
async fn set_room(
    State(app): State<App>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(body): Json<RoomFor>,
) -> Response {
    if owner(&app, &headers).is_none() {
        return not_for_them();
    }
    match app.rooms.set_grant(&name, &body.room, body.allowed) {
        Ok(_) => {
            info!(person = %name, room = %body.room, allowed = body.allowed, "the owner said who may go where");
            // Whoever it was sees the room appear or go from their list
            // at once, wherever they are sitting.
            app.rooms.announce();
            Json(json!({ "rooms": app.rooms.grants_of(&name).unwrap_or_default() })).into_response()
        }
        Err(err) => said(err),
    }
}

#[derive(Deserialize)]
struct RoomName {
    title: String,
}

/// Make a room.
async fn make_room(
    State(app): State<App>,
    headers: HeaderMap,
    Json(body): Json<RoomName>,
) -> Response {
    if owner(&app, &headers).is_none() {
        return not_for_them();
    }
    match app.rooms.make_room(&body.title) {
        Ok(id) => Json(json!({ "id": id })).into_response(),
        Err(err) => said(err),
    }
}

/// Call a room something else.
async fn rename_room(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<RoomName>,
) -> Response {
    if owner(&app, &headers).is_none() {
        return not_for_them();
    }
    match app.rooms.rename_room(&id, &body.title) {
        Ok(title) => Json(json!({ "id": id, "title": title })).into_response(),
        Err(err) => said(err),
    }
}

/// Take a room out of the house.
async fn delete_room(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if owner(&app, &headers).is_none() {
        return not_for_them();
    }
    match app.rooms.delete_room(&id) {
        Ok(true) => Json(json!({ "gone": id })).into_response(),
        Ok(false) => said(anyhow::anyhow!("there is no such room")),
        Err(err) => said(err),
    }
}

/// Join Audio: trade the invite link for this browser's own cookie.
/// The link is spent here and nowhere else, and this is the one request
/// in the room that gives a browser anything to keep -- which is the
/// whole reason the token travels in a body until now rather than in a
/// cookie the moment the link was opened.
async fn door_join(
    State(app): State<App>,
    headers: HeaderMap,
    Json(at): Json<AtTheDoor>,
) -> Response {
    let Some(t) = at.token else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "no invite link" })),
        )
            .into_response();
    };
    let Some((id, invite)) = app.any().claim(&t) else {
        return (
            StatusCode::GONE,
            Json(json!({ "state": "expired", "error": "This link is expired." })),
        )
            .into_response();
    };
    info!(person = %invite.name, called = %invite.display, "came in on their invite link");
    (
        [(
            header::SET_COOKIE,
            set_cookie(COOKIE, &id, SESSION_MAX_AGE, &headers),
        )],
        Json(json!({ "state": "in", "display": invite.display })),
    )
        .into_response()
}

/// Log out: forget this browser, and hand the cookie back cleared.
/// Nobody else's is touched, and the invite stays spent -- coming back
/// takes a new link.
///
/// There is one cookie to give back, and afterwards this browser is a
/// browser the room has never heard of. A link still open in the
/// address bar is not the room's to take away, and the door greets them
/// with it -- which is how somebody in as one person uses another's
/// link on the same browser.
async fn door_logout(State(app): State<App>, headers: HeaderMap) -> Response {
    if let Some(id) = cookie(&headers, COOKIE) {
        if let Some(who) = app.any().person_for_session(&id) {
            info!(person = %who.name, "logged out of a browser");
        }
        app.any().log_out(&id);
    }
    (
        [(header::SET_COOKIE, set_cookie(COOKIE, "", 0, &headers))],
        Json(json!({ "state": "none" })),
    )
        .into_response()
}

#[derive(Deserialize)]
struct Rename {
    display: String,
    /// The link this page is holding, for somebody naming themselves
    /// before they have spent it and so before they have a cookie.
    #[serde(default)]
    token: Option<String>,
}

/// What they want the room to call them: at the door before the link is
/// spent, and from the settings page afterwards. Theirs either way.
///
/// Their invite's name doesn't move. It is the key the cookie hangs on,
/// what `vox_room uninvite` names, what a room of their own is theirs
/// by, and the owner's to choose -- so this is the only name anybody
/// here can change about themselves, and the only one anybody reads.
///
/// This browser's own cookie comes first, and the link its page is
/// holding only counts when there is no cookie: a page holding somebody
/// else's link must not be able to rename them. Both are taken, because
/// the name is wanted before the link is spent as well as long after.
async fn door_name(
    State(app): State<App>,
    headers: HeaderMap,
    Json(body): Json<Rename>,
) -> Response {
    let who = invited(&app, &headers).map(|p| p.name).or_else(|| {
        body.token
            .as_deref()
            .and_then(|t| app.any().invite_at_door(t))
            .map(|i| i.name)
    });
    let Some(name) = who else {
        return (
            StatusCode::GONE,
            Json(json!({ "error": "This link is expired." })),
        )
            .into_response();
    };
    let Some(was) = app.any().called(&name) else {
        return (
            StatusCode::GONE,
            Json(json!({ "error": "The room doesn't know you." })),
        )
            .into_response();
    };
    // Written down once, then every room moves what it is holding under
    // the old name -- its roster, its faders, its conversation -- and
    // every tab in one is told to do the same.
    match app.any().rename(&name, &body.display) {
        Ok(display) => {
            app.rooms.renamed(&was, &display);
            // The desktop relay keys everything by the same name, and
            // it is not a room: a share of theirs would otherwise go on
            // being somebody who is no longer on any roster. Theirs
            // keeps running -- this is a rename, not an interruption.
            app.screen.renamed(&was, &display);
            Json(json!({ "display": display })).into_response()
        }
        Err(why) => (StatusCode::CONFLICT, Json(json!({ "error": why }))).into_response(),
    }
}

#[derive(Deserialize)]
struct SetLanguage {
    language: String,
    /// As for the name: chosen at the door, before there is a cookie to
    /// ask about.
    #[serde(default)]
    token: Option<String>,
}

/// The language they speak and want to hear, chosen at the door. Unlike
/// the name it is not spent with the link, so it can be changed later by
/// a browser the room already knows -- which is why both the link a page
/// is holding and an established cookie are accepted here.
async fn door_language(
    State(app): State<App>,
    headers: HeaderMap,
    Json(body): Json<SetLanguage>,
) -> Response {
    let who = person(&app, &headers).or_else(|| {
        body.token
            .as_deref()
            .and_then(|t| app.any().invite_at_door(t))
            .map(|i| i.display)
    });
    let Some(who) = who else {
        return (
            StatusCode::GONE,
            Json(json!({ "error": "This link is expired." })),
        )
            .into_response();
    };
    // The same: stored once, and the other rooms pick it up from the
    // database, which is also how they learn what queue to feed their
    // tabs from.
    match app.any().set_language(&who, &body.language) {
        Ok(language) => {
            app.rooms.catch_up();
            Json(json!({ "language": language })).into_response()
        }
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
    match app.any().game_with_prompt(&name) {
        Some((game, prompt)) => {
            info!(person = %who, game = %game.name, "peeked at a game's prompt");
            Json(json!({ "name": game.name, "summary": game.summary, "prompt": prompt }))
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "no such game").into_response(),
    }
}

/// A room this person may be in, for the history requests. Resolved
/// the same way `enter_room` resolves one, so a room that isn't theirs
/// to walk into isn't theirs to read either.
fn their_room(app: &App, headers: &HeaderMap, id: &str) -> Option<(Invited, Arc<Room>)> {
    let who = invited(app, headers)?;
    let room = app.rooms.find(&who, id)?;
    Some((who, room))
}

/// Every conversation a room has had, newest first, with the live one
/// marked. One person asking about their own screen: nothing here is
/// sent to the room.
async fn history(State(app): State<App>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    let Some((_, room)) = their_room(&app, &headers, &id) else {
        return (StatusCode::NOT_FOUND, "no such room").into_response();
    };
    Json(json!({ "conversations": room.history() })).into_response()
}

/// One past conversation, to read. Read-only by its nature: nothing
/// can be said into a conversation the room has moved on from.
async fn read_conversation(
    State(app): State<App>,
    Path((id, conversation)): Path<(String, i64)>,
    headers: HeaderMap,
) -> Response {
    let Some((_, room)) = their_room(&app, &headers, &id) else {
        return (StatusCode::NOT_FOUND, "no such room").into_response();
    };
    match room.read_conversation(conversation) {
        Some(v) => Json(v).into_response(),
        None => (StatusCode::NOT_FOUND, "no such conversation").into_response(),
    }
}

/// A picture somebody dropped into the room: it becomes a card in the
/// conversation with the next number on it, which is then a picture
/// like any other -- the AI can be asked for it as somebody's face,
/// and the conversation keeps it.
///
/// The body is the PNG itself rather than a form or a base64 field:
/// there is one file and no other fields, and the bytes are what the
/// room stores. The page has already scaled a photograph down to the
/// size the room shows a picture at and re-encoded it as PNG, which is
/// why this takes only the one type -- everything that holds a picture
/// here, from the card to the face, is a PNG and was before any of
/// this.
///
/// The room checks the bytes and the size itself: a page is not where
/// a limit lives, and `curl` is a browser too.
async fn drop_picture(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<PictureName>,
    png: Bytes,
) -> Response {
    let Some((who, room)) = their_room(&app, &headers, &id) else {
        return (StatusCode::NOT_FOUND, "no such room").into_response();
    };
    match room.upload_picture(&who.display, &q.name, &png) {
        Ok(n) => Json(json!({ "picture": n })).into_response(),
        Err(why) => (StatusCode::CONFLICT, Json(json!({ "error": why }))).into_response(),
    }
}

/// Delete one, and everything said and dealt in it. A POST and not a
/// DELETE so it goes through the same same-site cookie as everything
/// else here does.
async fn forget_conversation(
    State(app): State<App>,
    Path((id, conversation)): Path<(String, i64)>,
    headers: HeaderMap,
) -> Response {
    let Some((who, room)) = their_room(&app, &headers, &id) else {
        return (StatusCode::NOT_FOUND, "no such room").into_response();
    };
    match room.forget_conversation(conversation) {
        Ok(()) => {
            info!(person = %who.name, room = %id, conversation, "deleted a conversation");
            Json(json!({ "conversations": room.history() })).into_response()
        }
        Err(why) => (StatusCode::CONFLICT, Json(json!({ "error": why }))).into_response(),
    }
}

/// The app's own files, embedded in the binary.
///
/// Unknown paths get the app, which has only the one page: its routes
/// (`/lobby`, `/room/<link>`) are the browser's, and the server has
/// never heard of them.
///
/// A missing file under `assets/` is not one of those. It is a stale
/// index asking for a hashed file this binary doesn't have, and handing
/// it the app instead answered 200 `text/html` where a module was
/// expected -- which the browser refuses to run, so the page came up
/// blank with a MIME error and nothing to say why. It is a plain 404
/// now, which is what it is.
/// The square shown beside everything somebody says.
///
/// `who` is the name the room calls them, which is what the
/// conversation carries and so what a browser has to ask by; the
/// unchanging key it is really kept under is looked up here. A person
/// first and then an AI, the same order `Room::set_avatar` resolves in.
///
/// The version in the query is both how this is cached and how an old
/// face is reached: each one has an address of its own that never
/// changes meaning, so Settings can show ten of them and a browser
/// holding any of them never has to ask again. Without a version this
/// is whichever they are wearing, which is asked for afresh each time.
async fn avatar(
    State(app): State<App>,
    headers: HeaderMap,
    Path(who): Path<String>,
    Query(q): Query<AvatarQuery>,
) -> Response {
    // Inside the house only. Not a secret worth much, and not
    // something to hand to whoever finds the address either.
    if invited(&app, &headers).is_none() {
        return (StatusCode::FORBIDDEN, "not invited").into_response();
    }
    // `v` asks for one particular face of theirs. Settings shows the
    // last ten that way, each at its own address; without it this is
    // the one they are wearing.
    let found = match app.any().avatar_png(&who, q.v) {
        Ok(found) => found,
        Err(err) => {
            warn!("read {who}'s face: {err:#}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "no").into_response();
        }
    };
    // Nobody's face is not an error: the app draws the tint their name
    // already has, and asks for this only when the roster said there
    // was one. A 404 is what it gets if that has since been taken away.
    let Some((png, at)) = found else {
        return (StatusCode::NOT_FOUND, "no face").into_response();
    };
    let tag = format!("\"{at:x}\"");
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|given| given.split(',').any(|t| t.trim() == tag))
    {
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, tag)]).into_response();
    }
    (
        [
            (header::CONTENT_TYPE, "image/png".to_string()),
            (header::CACHE_CONTROL, AVATAR_CACHE.to_string()),
            (header::ETAG, tag),
            // The bytes are the same for everybody, and who may have
            // them is not: a shared cache must not answer this for a
            // browser that never showed a cookie.
            (header::VARY, "Cookie".to_string()),
        ],
        png,
    )
        .into_response()
}

/// A picture one of the AIs made, in the conversation it was made in.
///
/// Immutable the moment it exists: a picture is never redrawn under
/// the same id -- another take is another picture with another number
/// -- so a browser that has one never has to ask again.
async fn picture(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    if invited(&app, &headers).is_none() {
        return (StatusCode::FORBIDDEN, "not invited").into_response();
    }
    let found = match app.any().picture_png(&id) {
        Ok(found) => found,
        Err(err) => {
            warn!("read a picture: {err:#}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "no").into_response();
        }
    };
    // Still being made, or never arrived. The card is drawing a
    // spinner either way and is told which by the room, not by this.
    let Some(png) = found else {
        return (StatusCode::NOT_FOUND, "not yet").into_response();
    };
    (
        [
            (header::CONTENT_TYPE, "image/png".to_string()),
            (header::CACHE_CONTROL, AVATAR_CACHE.to_string()),
            (header::ETAG, format!("\"{id}\"")),
            (header::VARY, "Cookie".to_string()),
        ],
        png,
    )
        .into_response()
}

/// A clip of somebody reading aloud, for the play button on its card.
///
/// Immutable like a picture, and for the same reason: a clip is never
/// recorded again under the same id -- another go is another clip with
/// another number -- so a browser that has one never asks twice.
///
/// Anyone invited may play it. It is what somebody said out loud in a
/// room where everybody could hear them, and the transcript of it is
/// already on every screen in that room.
async fn sample(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    if invited(&app, &headers).is_none() {
        return (StatusCode::FORBIDDEN, "not invited").into_response();
    }
    let found = match app.any().sample_wav(&id) {
        Ok(found) => found,
        Err(err) => {
            warn!("read a voice sample: {err:#}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "no").into_response();
        }
    };
    // Still being read, or never read at all. The card says which,
    // from what the room told it rather than from this.
    let Some(wav) = found else {
        return (StatusCode::NOT_FOUND, "not yet").into_response();
    };
    (
        [
            (header::CONTENT_TYPE, "audio/wav".to_string()),
            (header::CACHE_CONTROL, AVATAR_CACHE.to_string()),
            (header::ETAG, format!("\"{id}\"")),
            (header::VARY, "Cookie".to_string()),
        ],
        wav,
    )
        .into_response()
}

/// Who somebody is: what opens when their face is clicked.
///
/// Asked for when the modal opens rather than sent with the roster. A
/// bio is a paragraph and the roster goes out whenever anybody's
/// microphone moves.
async fn profile(State(app): State<App>, headers: HeaderMap, Path(who): Path<String>) -> Response {
    if invited(&app, &headers).is_none() {
        return (StatusCode::FORBIDDEN, "not invited").into_response();
    }
    match app.any().profile(&who) {
        Some(profile) => Json(profile).into_response(),
        None => (StatusCode::NOT_FOUND, "nobody here by that name").into_response(),
    }
}

/// The faces somebody has worn, newest first, for the row of them in
/// Settings. Yours only: what somebody else has looked like is not a
/// list anybody needs.
async fn faces(State(app): State<App>, headers: HeaderMap, Path(who): Path<String>) -> Response {
    let Some(me) = invited(&app, &headers) else {
        return (StatusCode::FORBIDDEN, "not invited").into_response();
    };
    if me.display != who {
        return (StatusCode::FORBIDDEN, "those are not yours").into_response();
    }
    match app.any().faces(&who) {
        Ok(faces) => Json(json!({ "faces": faces })).into_response(),
        Err(err) => (StatusCode::BAD_REQUEST, err).into_response(),
    }
}

/// Put one of your own past faces back on, by its version.
///
/// Yours and nobody else's, like taking one off. Choosing again from
/// what has already been made costs nothing and needs no AI, which is
/// why it is a button rather than something to ask for out loud.
async fn wear_avatar(
    State(app): State<App>,
    headers: HeaderMap,
    Path(who): Path<String>,
    Json(body): Json<WearAvatar>,
) -> Response {
    let Some(me) = invited(&app, &headers) else {
        return (StatusCode::FORBIDDEN, "not invited").into_response();
    };
    if me.display != who {
        return (StatusCode::FORBIDDEN, "that is not your face").into_response();
    }
    match app.any().wear_avatar(&who, body.at) {
        Ok(true) => Json(json!({ "worn": body.at })).into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "you have never worn that one").into_response(),
        Err(err) => (StatusCode::BAD_REQUEST, err).into_response(),
    }
}

/// Take your own face off, leaving the tint your name already has.
///
/// Yours and nobody else's -- not even the owner's to clear, and not
/// the AI's either. A face is a thing you sat through an interview for;
/// whoever can take it away can take away the wrong one.
async fn forget_avatar(
    State(app): State<App>,
    headers: HeaderMap,
    Path(who): Path<String>,
) -> Response {
    let Some(me) = invited(&app, &headers) else {
        return (StatusCode::FORBIDDEN, "not invited").into_response();
    };
    if me.display != who {
        return (StatusCode::FORBIDDEN, "that is not your face").into_response();
    }
    match app.any().clear_avatar(&who) {
        Ok(gone) => Json(json!({ "cleared": gone })).into_response(),
        Err(err) => (StatusCode::BAD_REQUEST, err).into_response(),
    }
}

async fn assets(headers: HeaderMap, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let hashed = path.starts_with(ASSETS);
    let file = match Assets::get(path) {
        Some(f) => f,
        // Nothing to fall back to, and nothing to fall back on.
        None if hashed => return (StatusCode::NOT_FOUND, "not found").into_response(),
        None => match Assets::get("index.html") {
            Some(f) => f,
            None => return (StatusCode::NOT_FOUND, "not found").into_response(),
        },
    };
    // Of the file actually being sent, not of the path that was asked
    // for: an unknown path is answered with the index, and two unknown
    // paths are the same answer.
    let tag = etag(file.metadata.sha256_hash());
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|given| given.split(',').any(|t| t.trim() == tag))
    {
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, tag)]).into_response();
    }
    // `hashed` is what was asked for rather than what is going out, and
    // here that is the same thing: a request under `assets/` either
    // found its file or 404'd above.
    let cache = if hashed { ASSET_CACHE } else { INDEX_CACHE };
    (
        [
            (header::CONTENT_TYPE, file.metadata.mimetype().to_string()),
            (header::CACHE_CONTROL, cache.to_string()),
            (header::ETAG, tag),
        ],
        file.data,
    )
        .into_response()
}

/// A quoted, strong validator for one embedded file. Eight bytes of its
/// SHA-256: this says two files are the same file, and nothing hangs on
/// it but whether the browser is sent bytes it already has.
fn etag(hash: [u8; 32]) -> String {
    let mut tag = String::with_capacity(19);
    tag.push('"');
    for b in &hash[..8] {
        tag.push_str(&format!("{b:02x}"));
    }
    tag.push('"');
    tag
}

/// Where a tab is asking to be seated. A browser whose socket dropped
/// was somewhere when it did, and only it knows where: the room let go
/// of the tab when the connection went. Without this the socket came
/// back in the lobby and the tab had to ask to be moved -- which is a
/// change of rooms, and ends a desktop it was sharing (see
/// [`crate::screen::Screen::unshare`]) over a moment of bad wifi.
///
/// Asking is not being given: it is looked up with [`Rooms::find`] like
/// any other room, so a tab naming a room that is not theirs is seated
/// in the lobby and told nothing about it.
#[derive(Deserialize)]
struct Seat {
    room: Option<String>,
}

async fn ws(
    State(app): State<App>,
    headers: HeaderMap,
    Query(seat): Query<Seat>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let Some(who) = invited(&app, &headers) else {
        return (StatusCode::UNAUTHORIZED, "no invite").into_response();
    };
    upgrade.on_upgrade(move |socket| async move {
        let id = app.next_id.fetch_add(1, Ordering::Relaxed);
        if let Err(e) = client(&app, socket, id, &who, seat.room.as_deref()).await {
            debug!(id, "client ended: {e:#}");
        }
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
    /// How loud this tab wants the interpreter. Not a strip: it is
    /// heard by whoever is listening in one language and by nobody
    /// else, so it is not the room's to set.
    Interpreter {
        gain: Option<f32>,
        mute: Option<bool>,
    },
    /// When one bot in this room answers.
    Mode {
        bot: String,
        mode: String,
    },
    /// Whether talking over one bot in this room cuts it off.
    Stops {
        bot: String,
        stops: String,
    },
    /// Take a seat in the playhouse, or put somebody in one: the
    /// audience, the director's box, or the stage. Only while a puppet
    /// show is on -- see `room::Seat`. `who` defaults to whoever sent
    /// it, which is what the bands in the browser use it for.
    Seat {
        #[serde(default)]
        who: Option<String>,
        seat: String,
    },
    /// Typed into the room rather than said to it.
    Text {
        text: String,
    },
    /// What to call one of this room's conversations: the one it is
    /// having, or one out of its history.
    NameConversation {
        conversation: i64,
        title: String,
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
    /// Go to another room. The socket stays exactly where it is.
    EnterRoom {
        id: String,
    },
}

/// Take a seat in a room: on its roster, and fed its mix down the
/// channel this connection already has.
fn enter(
    room: &Arc<Room>,
    person: &str,
    tab: u64,
    audio: &mpsc::Sender<Vec<u8>>,
) -> anyhow::Result<()> {
    room.join(person, tab);
    room.mixer
        .add_listener(tab, person, &room.language_for(person), audio.clone())
}

/// Give the seat up. The connection keeps its channel: it is about to
/// be plugged into another room, or it is going away.
fn part(room: &Arc<Room>, person: &str, tab: u64) {
    room.mixer.remove_listener(tab);
    room.leave(person, tab);
}

async fn client(
    app: &App,
    socket: WebSocket,
    id: u64,
    who: &Invited,
    seat: Option<&str>,
) -> anyhow::Result<()> {
    let name = who.display.clone();
    let rooms = app.rooms.clone();
    // The mic frames in and the mix out live on this channel for as long
    // as the socket does. Changing room re-plugs the far end of it into
    // another mixer; nothing here is torn down and nothing is dropped.
    let (audio_tx, mut audio_rx) = mpsc::channel::<Vec<u8>>(LISTENER_QUEUE);
    // Where it asked to sit, if that is a room of theirs; the lobby is
    // where a tab that has never been anywhere belongs.
    let home = seat
        .and_then(|id| rooms.find(who, id))
        .unwrap_or_else(|| rooms.home());
    enter(&home, &name, id, &audio_tx)?;
    app.screen.place(&name, &home.config().id);
    rooms.announce();
    // Which room this tab is in. The two halves of the socket both need
    // it -- one reads the room's events, the other pushes the mic into
    // it -- and only one of them changes it.
    let (here, mut moved) = watch::channel(home.clone());
    // Shared, because both halves of the socket can move this tab: the
    // one reading commands when they press a room, and the one watching
    // the house for when the room they are in stops being theirs.
    let here = Arc::new(here);
    let (here_cmd, here_news) = (here.clone(), here.clone());
    let audio_news = audio_tx.clone();
    let mut news = rooms.subscribe();
    let (mut out, mut inp) = socket.split();
    out.send(Message::Text(home.hello(&name).to_string()))
        .await?;
    out.send(Message::Text(rooms.directory(who).to_string()))
        .await?;
    info!(id, person = %name, room = %home.config().id, "client connected");

    let served = async {
        let send = async {
            let mut room = home.clone();
            let mut events = room.subscribe();
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
                    // Somebody arrived, left or changed rooms. Worked out
                    // per person, so a lagged ping is as good as a fresh
                    // one: it says look again, not what changed.
                    ping = news.recv() => {
                        if matches!(ping, Err(broadcast::error::RecvError::Closed)) {
                            return Ok(());
                        }
                        // The room they are in may have stopped being
                        // theirs while they sat in it: taken out of the
                        // house, or taken away from them. Nobody is left
                        // in one -- it is off every list, so there would
                        // be no way out of it but the door, and a room
                        // somebody may not enter is not one to leave them
                        // sitting in either.
                        if rooms.find(who, &room.config().id).is_none() {
                            let back = rooms.home();
                            part(&room, &name, id);
                            enter(&back, &name, id, &audio_news)?;
                            // Out of the room is out of the room: a
                            // desktop they were sharing into it stops,
                            // the same as if they had walked.
                            app.screen.unshare(&name);
                            app.screen.place(&name, &back.config().id);
                            info!(id, person = %name, was = %room.config().id, "their room is no longer theirs");
                            let _ = here_news.send(back);
                            rooms.announce();
                            // Said down this socket rather than through
                            // the room: the tab is between two rooms at
                            // this instant -- out of the one it was in
                            // and not yet listening to the one it is
                            // going to -- so anything broadcast now is
                            // said to nobody.
                            out.send(Message::Text(
                                json!({
                                    "type": "notice",
                                    "text": "That room isn't yours any more. You are back in the lobby.",
                                })
                                .to_string(),
                            ))
                            .await?;
                        }
                        out.send(Message::Text(rooms.directory(who).to_string())).await?;
                    }
                    // This tab has gone to another room. Its events come
                    // from there now, and everything it was showing --
                    // the conversation, the roster, the cards, the faders
                    // -- belonged to the room it has left, so it is sent
                    // the new one whole rather than patched.
                    //
                    // Subscribed before the snapshot, as on the way in,
                    // so nothing said between the two is lost.
                    changed = moved.changed() => {
                        if changed.is_err() {
                            return Ok(());
                        }
                        room = moved.borrow_and_update().clone();
                        events = room.subscribe();
                        out.send(Message::Text(room.hello(&name).to_string())).await?;
                    }
                }
            }
        };

        let recv = async {
            let mut decoder = opus::Decoder::new(RATE, opus::Channels::Mono)?;
            // 120 ms, the longest Opus frame.
            let mut pcm = vec![0.0f32; RATE as usize / 1000 * 120];
            while let Some(msg) = inp.next().await {
                // Whichever room this tab is in as the message arrives.
                let room = here.borrow().clone();
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
                        Ok(Command::Interpreter { gain, mute }) => {
                            room.set_interpreter(id, gain, mute)
                        }
                        Ok(Command::Mode { bot, mode }) => room.set_mode(&bot, &mode),
                        Ok(Command::Stops { bot, stops }) => room.set_stops(&bot, &stops),
                        // Only the sender hears about a message the room
                        // wouldn't take: it is their typing, not news.
                        Ok(Command::Text { text }) => {
                            if let Err(why) = room.typed(&name, &text) {
                                room.tell(&name, &why);
                            }
                        }
                        Ok(Command::NameConversation {
                            conversation,
                            title,
                        }) => {
                            if let Err(why) = room.name_conversation(conversation, &title) {
                                room.tell(&name, &why);
                            }
                        }
                        Ok(Command::Seat { who, seat }) => {
                            // Yourself unless you said otherwise. The
                            // seats are the room's, like the faders:
                            // anybody may move anybody, and the one
                            // that cannot be moved says so itself.
                            let who = who.unwrap_or_else(|| name.clone());
                            if let Err(why) = room.take_seat(&who, &seat) {
                                room.tell(&name, &why);
                            }
                        }
                        Ok(Command::Stop) => room.stop_agent(),
                        Ok(Command::NewConversation) => room.new_conversation(),
                        Ok(Command::EnterGame { name }) => room.enter_game(&name),
                        Ok(Command::Lobby) => room.lobby(),
                        Ok(Command::DeleteGame { name }) => room.delete_game(&name),
                        Ok(Command::EnterRoom { id: want }) => {
                            if want == room.config().id {
                                continue;
                            }
                            let Some(next) = rooms.find(who, &want) else {
                                room.tell(&name, "That room isn't here.");
                                continue;
                            };
                            part(&room, &name, id);
                            enter(&next, &name, id, &audio_tx)?;
                            // Their desktop does not go with them. It
                            // was shared to the room they just left,
                            // and sharing it here is a thing to decide
                            // on purpose.
                            app.screen.unshare(&name);
                            app.screen.place(&name, &next.config().id);
                            info!(id, person = %name, room = %want, "changed rooms");
                            let _ = here_cmd.send(next);
                            rooms.announce();
                        }
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
    .await;

    // Whichever room they ended up in is the one they are leaving.
    part(&here.borrow().clone(), &name, id);
    rooms.announce();
    served
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
