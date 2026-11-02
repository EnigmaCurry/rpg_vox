//! An MCP server the room can reach, and the tools it lends the AI.
//!
//! One kind of transport only: **streamable HTTP**. Every request is a
//! JSON-RPC 2.0 message posted to one URL, and the answer comes back
//! either as JSON or as a one-event SSE stream -- which is what servers
//! actually do, so [`rpc`] reads both. A stdio server would want a
//! child process to supervise for as long as the house is open, and a
//! room that is holding a conversation is the wrong place to be
//! reaping one.
//!
//! The calls happen on the agent's own thread, blocking, beside the
//! LLM stream and the TTS server that are already there.
//!
//! ## What the model is shown
//!
//! Not what the server offers. Two things are done to it first:
//!
//! 1. **Only the tools the room was configured to lend.** Retro
//!    Diffusion offers 33; a room that wants pictures needs four of
//!    them, and the other 29 are 29 more ways for a voice answering
//!    "make me an avatar" to go somewhere else. Which ones is the
//!    configuration's to say -- see [`Server::only`] -- because that
//!    is a decision about the room, not one to re-make per turn.
//! 2. **No key in the schema.** Most of Retro Diffusion's tools take
//!    an `rd_api_key` argument. A model shown a field like that will
//!    fill it in, with something it made up -- and a model that
//!    somehow filled it in correctly would have put the key in the
//!    transcript, which is written down and shown on screen. So
//!    [`AUTH_ARGS`] is stripped from every schema on the way out and
//!    put back on the way in, by [`Server::call`], from the key the
//!    room was started with.
//!
//! Names are prefixed with the server's own (`art_create_inference`),
//! so two servers lending a tool of the same name stay apart, and so
//! the room's own tools can never be shadowed by somebody else's.
//!
//! ## What comes back
//!
//! A tool result is a list of content blocks, and the ones that matter
//! here are text and images. The text goes back to the model as the
//! tool's answer. The pictures do not: a 128x128 PNG is some 30 KB of
//! base64, and putting that through a conversation that is read aloud
//! helps nobody. They are kept in a [`Stash`] instead, and the model
//! is told in words what it has -- "2 pictures, 128x128" -- which is
//! all it needs to hand one to `set_avatar`.
//!
//! A picture arrives one of three ways, and all three are real:
//!
//! - an `image` content block, base64 in the message;
//! - `base64_images` in the result's own JSON;
//! - `output_urls`, and nothing inline at all -- which is what Retro
//!   Diffusion does over MCP, where `upload_outputs` defaults on and
//!   the raw payloads are deliberately left out to keep the result
//!   small. Those are fetched here.

use std::collections::HashMap;
use std::io::Read as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context as _, Result};
use base64::Engine as _;
use serde_json::{json, Value};
use tracing::{debug, info, warn};

/// The protocol version this client speaks. A server that wants another
/// says so in its `initialize` answer, and is taken at its word: the
/// version is only ever sent back, never compared.
const PROTOCOL: &str = "2025-06-18";

/// How long any one call may take. Generous because the slow one is
/// generating a picture, which holds the connection while it runs.
const TIMEOUT: Duration = Duration::from_secs(120);
/// How long listing tools at startup may take. Short: the house is
/// opening, and a server that cannot say hello promptly is one the room
/// carries on without.
const HELLO_TIMEOUT: Duration = Duration::from_secs(20);

/// Arguments that are a key rather than a thing to decide. Stripped
/// from every schema the model is shown, and filled in from the
/// configuration when the call is made.
///
/// Retro Diffusion's is `rd_api_key`; the rest are what other servers
/// call the same thing. There is no way to ask a server which of its
/// arguments is a secret, so this is a list and not a rule.
const AUTH_ARGS: &[&str] = &[
    "rd_api_key",
    "api_key",
    "apiKey",
    "access_token",
    "accessToken",
    "token",
    "secret",
];

/// The most pictures kept from one call, and the most bytes one may be.
/// A picture bigger than this is not an avatar, and the room says so
/// rather than putting it in the database.
const MAX_PICTURES: usize = 8;
const MAX_BYTES: usize = 4 * 1024 * 1024;

/// A picture an MCP tool made, waiting to be given to somebody.
#[derive(Clone)]
pub struct Picture {
    pub png: Vec<u8>,
    /// What it was asked for: the `prompt` argument of the call that
    /// made it, when the call had one. It is kept with the avatar, so
    /// the next one can be asked for in the same spirit.
    pub prompt: String,
}

/// The pictures the last call made, per bot.
///
/// Per bot and not per room: two AIs in one room generating at once
/// must not be able to hand each other's picture to `set_avatar`. One
/// call's worth each, replaced by the next -- a bot that generates
/// again has changed its mind, and the one before it was never
/// anybody's face.
#[derive(Default)]
pub struct Stash(Mutex<std::collections::HashMap<String, Vec<Picture>>>);

impl Stash {
    fn put(&self, bot: &str, pictures: Vec<Picture>) {
        self.0
            .lock()
            .expect("stash")
            .insert(bot.to_string(), pictures);
    }

    /// The `nth` picture (1-based) this bot last made, or None when it
    /// has made none or asked for one that isn't there.
    pub fn take(&self, bot: &str, nth: usize) -> Option<Picture> {
        let all = self.0.lock().expect("stash");
        let mine = all.get(bot)?;
        mine.get(nth.checked_sub(1)?).cloned()
    }

    /// How many this bot is holding, for telling it what it has.
    pub fn held(&self, bot: &str) -> usize {
        self.0
            .lock()
            .expect("stash")
            .get(bot)
            .map_or(0, |v| v.len())
    }
}

/// How a server is named and reached. What the configuration says; see
/// `main.rs` for the flag that builds one.
#[derive(Clone, Debug)]
pub struct Config {
    /// What the room calls it, and the prefix on the tools it lends.
    /// Short, because it is in front of every tool name the model
    /// reads: `art`, not `retro_diffusion_image_generation`.
    pub name: String,
    pub url: String,
    /// The key, sent as a bearer token and filled into whichever
    /// [`AUTH_ARGS`] a tool declares. Empty for a server that wants
    /// none.
    pub key: String,
    /// The tools to lend, by their name on the server. Empty lends
    /// every tool it has, which is for a server you know is small.
    pub only: Vec<String>,
}

pub struct Server {
    cfg: Config,
    http: ureq::Agent,
    /// The tools it lends, as the model is shown them: prefixed, and
    /// with the key taken out. Filled in by [`Server::connect`], and
    /// empty until then or if that failed -- a room whose picture
    /// server is down is a room that cannot make pictures, and
    /// otherwise carries on.
    offer: Mutex<Vec<Value>>,
    /// Which arguments of each tool are the key, by its bare name.
    /// Taken out of the schema the model sees and kept here, because
    /// the offered schema is sent to the LLM endpoint verbatim and an
    /// endpoint that validates its tool definitions rejects a field it
    /// has never heard of.
    auth: Mutex<HashMap<String, Vec<String>>>,
    /// The session a server asked us to keep, echoed on every later
    /// request. Retro Diffusion hands out none and wants none; the
    /// protocol allows one, and a server that uses them would
    /// otherwise see every call as a stranger.
    session: Mutex<Option<String>>,
    next_id: AtomicU64,
}

impl Server {
    pub fn new(cfg: Config) -> Self {
        Self {
            cfg,
            http: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(10))
                .build(),
            offer: Mutex::new(Vec::new()),
            auth: Mutex::new(HashMap::new()),
            session: Mutex::new(None),
            next_id: AtomicU64::new(1),
        }
    }

    pub fn name(&self) -> &str {
        &self.cfg.name
    }

    /// The key this server was built with, for the test in `main.rs`
    /// that checks which key went to which server. Not otherwise
    /// reachable: nothing in the room has any business reading it
    /// back out.
    #[cfg(test)]
    pub fn key_for_test(&self) -> &str {
        &self.cfg.key
    }

    /// Say hello and find out what it has. Called once as the house
    /// opens, on its own thread: a server that is slow to answer must
    /// not hold up a room that works without it.
    ///
    /// The answer is how many tools it lends, which is what the log
    /// line says. An error here is not fatal anywhere -- see
    /// [`Server::offer`].
    pub fn connect(&self) -> Result<usize> {
        let hello = self
            .rpc(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL,
                    "capabilities": {},
                    "clientInfo": { "name": "vox_room", "version": env!("CARGO_PKG_VERSION") },
                }),
                HELLO_TIMEOUT,
            )
            .with_context(|| format!("say hello to {}", self.cfg.url))?;
        let server = hello["serverInfo"]["name"].as_str().unwrap_or("?");
        let version = hello["serverInfo"]["version"].as_str().unwrap_or("?");
        // The handshake is not over until the server has been told the
        // client is ready. A notification: no id, and no answer.
        self.notify("notifications/initialized", json!({}))?;

        let listed = self.rpc("tools/list", json!({}), HELLO_TIMEOUT)?;
        let tools = listed["tools"].as_array().cloned().unwrap_or_default();
        let mut offer = Vec::new();
        let mut auth = HashMap::new();
        let mut skipped = Vec::new();
        for t in &tools {
            let Some(name) = t["name"].as_str() else {
                continue;
            };
            if !self.lends(name) {
                skipped.push(name.to_string());
                continue;
            }
            let keys = auth_args_in(&t["inputSchema"]);
            if !keys.is_empty() {
                auth.insert(name.to_string(), keys);
            }
            offer.push(self.as_offered(name, t));
        }
        // A name in the configuration that the server has never heard
        // of is a typo, and a typo here is a tool the room thinks it
        // has. Said once, loudly, rather than discovered mid-sentence.
        for want in &self.cfg.only {
            if !tools.iter().any(|t| t["name"].as_str() == Some(want)) {
                warn!(
                    server = self.cfg.name,
                    tool = want,
                    "{server} has no such tool, so the room won't have it either"
                );
            }
        }
        let lent = offer.len();
        *self.offer.lock().expect("offer") = offer;
        *self.auth.lock().expect("auth") = auth;
        info!(
            server = self.cfg.name,
            url = self.cfg.url,
            "{server} {version}: lending {lent} of its {} tools{}",
            tools.len(),
            if skipped.len() > 6 {
                String::new()
            } else {
                format!(", not {}", skipped.join(", "))
            }
        );
        Ok(lent)
    }

    /// Whether `tool` is one the room lends out.
    fn lends(&self, tool: &str) -> bool {
        self.cfg.only.is_empty() || self.cfg.only.iter().any(|t| t == tool)
    }

    /// One of the server's tools, in the shape the endpoint wants and
    /// with nothing in it the model should not be deciding: prefixed,
    /// and with the key gone from both the properties and the required
    /// list. See the module docs.
    fn as_offered(&self, name: &str, tool: &Value) -> Value {
        let mut schema = tool["inputSchema"].clone();
        if !schema.is_object() {
            schema = json!({ "type": "object", "properties": {} });
        }
        if let Some(props) = schema["properties"].as_object_mut() {
            for arg in AUTH_ARGS {
                props.remove(*arg);
            }
        }
        if let Some(req) = schema["required"].as_array() {
            let kept: Vec<Value> = req
                .iter()
                .filter(|r| !r.as_str().is_some_and(|r| AUTH_ARGS.contains(&r)))
                .cloned()
                .collect();
            schema["required"] = Value::Array(kept);
        }
        json!({
            "type": "function",
            "function": {
                "name": format!("{}_{name}", self.cfg.name),
                "description": tool["description"].as_str().unwrap_or(""),
                "parameters": schema,
            }
        })
    }

    /// The tools to put in front of the model, or nothing when this
    /// server never answered.
    pub fn offer(&self) -> Vec<Value> {
        self.offer.lock().expect("offer").clone()
    }

    /// Whether `tool` is one of this server's, by the prefixed name the
    /// model calls it by.
    pub fn has(&self, tool: &str) -> bool {
        self.offer
            .lock()
            .expect("offer")
            .iter()
            .any(|t| t["function"]["name"].as_str() == Some(tool))
    }

    /// Call one of this server's tools, by the prefixed name the model
    /// used. The answer is what to tell the model; any pictures it made
    /// are left in `stash` under `bot`.
    pub fn call(&self, bot: &str, tool: &str, args: &Value, stash: &Stash) -> Result<String> {
        let bare = tool
            .strip_prefix(&format!("{}_", self.cfg.name))
            .ok_or_else(|| anyhow!("{tool} is not {}'s", self.cfg.name))?;
        if !self.lends(bare) {
            bail!("{tool} is not a tool this room lends");
        }
        let mut args = match args {
            Value::Object(_) => args.clone(),
            // A model that called with nothing, or with a string it
            // meant as JSON: an empty object is what the server wants
            // and what its own validation will then complain about,
            // which is a better error than one from here.
            _ => json!({}),
        };
        // The key goes in now, into whichever argument this tool calls
        // it -- which is known from the schema the server published,
        // not guessed. Then the bearer header carries it as well, for
        // the servers that want it there instead.
        if !self.cfg.key.is_empty() {
            if let Some(obj) = args.as_object_mut() {
                for arg in self.auth_args_of(bare) {
                    obj.insert(arg, Value::from(self.cfg.key.clone()));
                }
            }
        }
        let what = args["prompt"].as_str().unwrap_or_default().to_string();
        let result = self.rpc(
            "tools/call",
            json!({ "name": bare, "arguments": args }),
            TIMEOUT,
        )?;

        // A tool that failed says so in the result rather than as a
        // JSON-RPC error: the call worked, the tool didn't.
        let failed = result["isError"].as_bool().unwrap_or(false);
        let text = self.text_of(&result);
        if failed {
            bail!(
                "{bare} failed: {}",
                if text.is_empty() {
                    "no reason given".into()
                } else {
                    text
                }
            );
        }

        let pictures = self.pictures_of(&result, &what);
        if pictures.is_empty() {
            return Ok(if text.is_empty() {
                format!("{bare} is done")
            } else {
                text
            });
        }
        let how_many = pictures.len();
        let sizes = pictures
            .iter()
            .map(|p| format!("{} KB", p.png.len() / 1024))
            .collect::<Vec<_>>()
            .join(", ");
        stash.put(bot, pictures);
        // Deliberately not the pictures, nor where they are: see the
        // module docs. What the model needs is that they exist and how
        // to refer to one.
        Ok(format!(
            "{how_many} picture{} ({sizes}), kept as {}. Put one on somebody with \
             set_avatar, whose `picture` is which of these to use, counting from 1. \
             Generating again replaces them.{}",
            if how_many == 1 { "" } else { "s" },
            if how_many == 1 {
                "picture 1".to_string()
            } else {
                format!("pictures 1 to {how_many}")
            },
            if text.is_empty() {
                String::new()
            } else {
                format!(" The tool also said: {text}")
            }
        ))
    }

    /// Which of this tool's arguments are the key, by what the server
    /// published. A tool that declares none gets none -- putting an
    /// `api_key` into a call that never asked for one is how a strict
    /// server rejects everything.
    fn auth_args_of(&self, bare: &str) -> Vec<String> {
        self.auth
            .lock()
            .expect("auth")
            .get(bare)
            .cloned()
            .unwrap_or_default()
    }

    /// Everything the result said in words, which is what the model is
    /// told. Image blocks are not words and are left out.
    fn text_of(&self, result: &Value) -> String {
        result["content"]
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|b| b["type"] == "text")
                    .filter_map(|b| b["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
                    .trim()
                    .to_string()
            })
            .unwrap_or_default()
    }

    /// The pictures in a result, however it put them there. All three
    /// ways are real; see the module docs.
    fn pictures_of(&self, result: &Value, prompt: &str) -> Vec<Picture> {
        let mut out: Vec<Picture> = Vec::new();
        let keep = |out: &mut Vec<Picture>, png: Vec<u8>| {
            if out.len() >= MAX_PICTURES {
                return;
            }
            if png.len() > MAX_BYTES {
                warn!(bytes = png.len(), "a picture too big to keep was dropped");
                return;
            }
            out.push(Picture {
                png,
                prompt: prompt.to_string(),
            });
        };

        // 1. Inline, as the protocol's own image block.
        for b in result["content"].as_array().into_iter().flatten() {
            if b["type"] != "image" {
                continue;
            }
            match b["data"].as_str().map(decode) {
                Some(Ok(png)) => keep(&mut out, png),
                Some(Err(err)) => warn!("an image block wouldn't decode: {err:#}"),
                None => {}
            }
        }

        // 2 and 3. In the tool's own JSON, which arrives as
        // `structuredContent` or as JSON inside the text -- a server
        // that declares an output schema sends the first, and one that
        // doesn't sends the second, and the fields are the same either
        // way.
        let mut bodies = Vec::new();
        if result["structuredContent"].is_object() {
            bodies.push(result["structuredContent"].clone());
        }
        if let Ok(v) = serde_json::from_str::<Value>(&self.text_of(result)) {
            bodies.push(v);
        }
        for body in bodies {
            for b64 in body["base64_images"].as_array().into_iter().flatten() {
                match b64.as_str().map(decode) {
                    Some(Ok(png)) => keep(&mut out, png),
                    Some(Err(err)) => warn!("base64_images wouldn't decode: {err:#}"),
                    None => {}
                }
            }
            // Only worth fetching when nothing came inline: a server
            // that sent both sent the same pictures twice.
            if !out.is_empty() {
                continue;
            }
            for url in body["output_urls"].as_array().into_iter().flatten() {
                let Some(url) = url.as_str() else { continue };
                match self.fetch(url) {
                    Ok(png) => keep(&mut out, png),
                    Err(err) => warn!("couldn't fetch a generated picture: {err:#}"),
                }
            }
        }
        out
    }

    /// Fetch a hosted result. The URLs are signed and short-lived, so
    /// this happens now or not at all.
    fn fetch(&self, url: &str) -> Result<Vec<u8>> {
        if !url.starts_with("https://") {
            bail!("not an https URL");
        }
        let resp = self
            .http
            .get(url)
            .timeout(TIMEOUT)
            .call()
            .with_context(|| format!("GET {url}"))?;
        let mut png = Vec::new();
        resp.into_reader()
            .take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut png)
            .context("read the picture")?;
        Ok(png)
    }

    /// One JSON-RPC call: post it, read the answer, hand back `result`.
    fn rpc(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let resp = self
            .post(timeout)
            .send_json(&body)
            .map_err(|e| self.wherefore(method, e))?;
        // A server that keeps sessions says so once, in the answer to
        // `initialize`, and expects it back from then on.
        if let Some(sid) = resp.header("mcp-session-id") {
            *self.session.lock().expect("session") = Some(sid.to_string());
        }
        let sse = resp
            .header("content-type")
            .is_some_and(|c| c.contains("text/event-stream"));
        let text = resp.into_string().context("read the answer")?;
        let text = if sse { events(&text) } else { text };
        let answer: Value = serde_json::from_str(&text)
            .with_context(|| format!("{method}: {}", snippet(&text)))
            .context("the answer wasn't JSON")?;
        if let Some(err) = answer["error"].as_object() {
            let message = err["message"].as_str().unwrap_or("no message");
            let code = err["code"].as_i64().unwrap_or(0);
            bail!("{method} was refused: {message} ({code})");
        }
        if answer["result"].is_null() {
            bail!("{method}: the answer had no result: {}", snippet(&text));
        }
        Ok(answer["result"].clone())
    }

    /// Tell the server something, with no answer expected. The reply to
    /// one of these is an empty 202, and anything in it is nothing to
    /// read.
    fn notify(&self, method: &str, params: Value) -> Result<()> {
        let body = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.post(HELLO_TIMEOUT)
            .send_json(&body)
            .map_err(|e| self.wherefore(method, e))?;
        Ok(())
    }

    /// A request with everything this server wants on it. `Accept` has
    /// both kinds on purpose: the answer may be either, and which one
    /// it is is read off the response.
    fn post(&self, timeout: Duration) -> ureq::Request {
        let mut req = self
            .http
            .post(&self.cfg.url)
            .timeout(timeout)
            .set("content-type", "application/json")
            .set("accept", "application/json, text/event-stream")
            .set("mcp-protocol-version", PROTOCOL);
        if !self.cfg.key.is_empty() {
            req = req.set("authorization", &format!("Bearer {}", self.cfg.key));
        }
        if let Some(sid) = self.session.lock().expect("session").as_deref() {
            req = req.set("mcp-session-id", sid);
        }
        req
    }

    /// What went wrong, with the body in it. ureq's own message for a
    /// failing status says the status and nothing else, and the reason
    /// is always in the body.
    fn wherefore(&self, method: &str, e: ureq::Error) -> anyhow::Error {
        match e {
            ureq::Error::Status(code, resp) => {
                let body = resp.into_string().unwrap_or_default();
                let body = if let Ok(v) = serde_json::from_str::<Value>(&body) {
                    v["error"]["message"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| snippet(&body))
                } else {
                    snippet(&body)
                };
                // The one worth spelling out: a key that is missing or
                // wrong looks like every other HTTP error otherwise,
                // and it is the one that will actually happen.
                let hint = match code {
                    401 | 403 => " -- check the key the room was started with",
                    402 => " -- the account is out of balance",
                    _ => "",
                };
                anyhow!("{method}: HTTP {code}: {body}{hint}")
            }
            e => anyhow!("{method}: {e}"),
        }
    }
}

/// Which of a published schema's arguments are a key rather than a
/// thing to decide. See [`AUTH_ARGS`].
fn auth_args_in(schema: &Value) -> Vec<String> {
    schema["properties"]
        .as_object()
        .map(|props| {
            AUTH_ARGS
                .iter()
                .filter(|a| props.contains_key(**a))
                .map(|a| a.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The JSON out of an SSE body: the `data:` lines, joined.
///
/// A streamable-HTTP server may answer a single request with a stream
/// of one event, and Retro Diffusion does -- its `content-type` is
/// `text/event-stream` even for `initialize`. One event is all there
/// ever is for a request with an id, so this takes the data lines and
/// leaves the framing.
fn events(body: &str) -> String {
    let mut out = String::new();
    for line in body.lines() {
        if let Some(data) = line.strip_prefix("data:") {
            out.push_str(data.trim_start());
        }
    }
    out
}

/// Base64, in either alphabet. A picture is standard base64, but a
/// server that signed a URL into one of its fields may have used the
/// URL-safe one, and guessing wrong loses the picture.
fn decode(s: &str) -> Result<Vec<u8>> {
    let s = s.trim();
    // A data: URI is not what the protocol asks for, and is sent
    // anyway. The payload is after the comma.
    let s = s.rsplit_once("base64,").map_or(s, |(_, rest)| rest);
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(s))
        .map_err(|e| anyhow!("{e}"))
}

/// Enough of a body to say what happened, and not enough to fill the
/// log with somebody's picture.
fn snippet(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() <= 300 {
        return s.to_string();
    }
    format!("{}…", s.chars().take(300).collect::<String>())
}

/// Whether these bytes are a PNG. The store keeps what it is given and
/// the browser is told it is a PNG, so this is where that is made true.
pub fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a])
}

/// Every server the house was started with, said hello to on its own
/// thread.
///
/// The threads are deliberately not waited for. A picture server that
/// is slow or down must not keep the rooms from opening -- what it
/// costs is that the AI has no picture tools until it answers, and the
/// log says which server and why.
pub fn connect_all(servers: &[Arc<Server>]) {
    for server in servers {
        let server = server.clone();
        std::thread::Builder::new()
            .name(format!("mcp-{}", server.name()))
            .spawn(move || {
                if let Err(err) = server.connect() {
                    warn!(
                        server = server.name(),
                        "no tools from this one: {err:#}; the room carries on without it"
                    );
                } else {
                    debug!(server = server.name(), "ready");
                }
            })
            .expect("spawn an mcp thread");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(only: &[&str]) -> Server {
        Server::new(Config {
            name: "art".into(),
            url: "https://example.invalid/mcp".into(),
            key: "rdpk-secret".into(),
            only: only.iter().map(|s| s.to_string()).collect(),
        })
    }

    /// The key is never in what the model is shown -- not as a
    /// property, and not as something the call is required to have.
    #[test]
    fn the_model_is_never_shown_the_key() {
        let s = server(&[]);
        let offered = s.as_offered(
            "create_inference",
            &json!({
                "name": "create_inference",
                "description": "Generate images.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "prompt": { "type": "string" },
                        "width": { "type": "integer" },
                        "rd_api_key": { "type": "string" },
                    },
                    "required": ["prompt", "rd_api_key"],
                },
            }),
        );
        let schema = &offered["function"]["parameters"];
        assert!(schema["properties"]["prompt"].is_object(), "the real ones stay");
        assert!(
            schema["properties"]["rd_api_key"].is_null(),
            "and the key is gone"
        );
        assert_eq!(
            schema["required"].as_array().unwrap(),
            &[json!("prompt")],
            "and is not required either"
        );
        let whole = serde_json::to_string(&offered).unwrap();
        assert!(!whole.contains("rdpk-secret"), "nor the key itself: {whole}");
        assert_eq!(
            offered["function"]["name"], "art_create_inference",
            "and it is named for the server it came from"
        );
    }

    /// A room lends the tools it was configured to lend and no others,
    /// whatever the server offers.
    #[test]
    fn only_the_configured_tools_are_lent() {
        let s = server(&["create_inference", "get_balance"]);
        assert!(s.lends("create_inference"));
        assert!(s.lends("get_balance"));
        assert!(
            !s.lends("start_lowpoly_generate"),
            "33 tools offered, two lent"
        );
        // An empty list is the other way round, for a small server.
        assert!(server(&[]).lends("anything"));
    }

    /// A one-event SSE body is a JSON-RPC answer with framing around
    /// it. This is the shape Retro Diffusion actually replies with.
    #[test]
    fn an_sse_answer_is_read_as_json() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n\n";
        let v: Value = serde_json::from_str(&events(body)).unwrap();
        assert_eq!(v["result"]["ok"], true);
        // And a plain JSON body is left alone by the same path.
        assert_eq!(events("data: {\"a\":1}").trim(), "{\"a\":1}");
    }

    #[test]
    fn pictures_come_inline_or_not_at_all() {
        let s = server(&[]);
        let png = {
            let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
            v.extend_from_slice(b"body");
            v
        };
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);

        // An image content block.
        let got = s.pictures_of(
            &json!({ "content": [{ "type": "image", "data": b64, "mimeType": "image/png" }] }),
            "a brass head",
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].png, png);
        assert_eq!(got[0].prompt, "a brass head", "kept with what made it");
        assert!(is_png(&got[0].png));

        // base64_images in the tool's own JSON, as structuredContent.
        let got = s.pictures_of(
            &json!({ "structuredContent": { "base64_images": [b64] } }),
            "",
        );
        assert_eq!(got.len(), 1);

        // And the same, as JSON inside the text block.
        let got = s.pictures_of(
            &json!({ "content": [{ "type": "text",
                "text": format!("{{\"base64_images\":[\"{b64}\"]}}") }] }),
            "",
        );
        assert_eq!(got.len(), 1);

        // Nothing at all: a tool that only said something.
        assert!(s
            .pictures_of(&json!({ "content": [{ "type": "text", "text": "balance: $1.00" }] }), "")
            .is_empty());
    }

    /// One call's worth per bot, and one bot cannot reach another's.
    #[test]
    fn a_stash_is_the_last_call_and_is_each_bots_own() {
        let stash = Stash::default();
        let pic = |p: &str| Picture {
            png: vec![1],
            prompt: p.into(),
        };
        stash.put("Vortex Prime", vec![pic("first"), pic("second")]);
        assert_eq!(stash.held("Vortex Prime"), 2);
        assert_eq!(stash.take("Vortex Prime", 1).unwrap().prompt, "first");
        assert_eq!(stash.take("Vortex Prime", 2).unwrap().prompt, "second");
        assert!(stash.take("Vortex Prime", 3).is_none());
        assert!(stash.take("Vortex Prime", 0).is_none(), "counting from 1");
        assert!(stash.take("Someone Else", 1).is_none(), "not shared");

        // Generating again replaces them rather than piling up.
        stash.put("Vortex Prime", vec![pic("latest")]);
        assert_eq!(stash.held("Vortex Prime"), 1);
        assert_eq!(stash.take("Vortex Prime", 1).unwrap().prompt, "latest");
    }

    /// The real thing: say hello to Retro Diffusion's server and read
    /// what it lends back. No key, because `initialize` and
    /// `tools/list` want none -- which makes this a free check of the
    /// whole risky half (the SSE framing, the handshake, the
    /// filtering, the key never reaching the schema) against a server
    /// nobody here controls.
    ///
    /// ```text
    /// cargo test -p vox_room -- --ignored --nocapture says_hello
    /// ```
    #[test]
    #[ignore = "talks to Retro Diffusion's MCP server over the network"]
    fn says_hello_to_a_real_server() {
        let s = Server::new(Config {
            name: "art".into(),
            url: "https://mcp.retrodiffusion.ai/mcp".into(),
            key: String::new(),
            only: vec![
                "create_inference".into(),
                "list_available_styles".into(),
                "get_balance".into(),
            ],
        });
        let lent = s.connect().expect("say hello");
        assert_eq!(lent, 3, "three of its 33, because three is what was asked for");

        let offer = s.offer();
        let names: Vec<&str> = offer
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"art_create_inference"), "{names:?}");
        assert!(!names.iter().any(|n| n.contains("lowpoly")), "{names:?}");

        // The thing that must be true of a real schema and cannot be
        // checked against a made-up one: its key argument is real, it
        // is gone from what the model sees, and the room knows where
        // to put it back.
        let whole = serde_json::to_string(&offer).unwrap();
        assert!(!whole.contains("rd_api_key"), "the key is not in the offer");
        assert_eq!(
            s.auth_args_of("create_inference"),
            ["rd_api_key"],
            "and the room remembers which argument it was"
        );
        // Every tool it lends is a shape an endpoint will accept.
        for t in &offer {
            assert_eq!(t["type"], "function");
            assert!(t["function"]["parameters"]["type"] == "object", "{t}");
            assert!(
                !t["function"]["description"].as_str().unwrap_or("").is_empty(),
                "{t}"
            );
        }
    }

    /// The one link the free test above cannot reach: a real
    /// `tools/call`, with the key put back into the argument it was
    /// stripped out of. `get_balance` is free and changes nothing, so
    /// this costs a request and no money.
    ///
    /// ```text
    /// VOX_ROOM_MCP_KEY=rdpk-… cargo test -p vox_room -- --ignored --nocapture the_key_is_put_back
    /// ```
    #[test]
    #[ignore = "needs a Retro Diffusion key in VOX_ROOM_MCP_KEY"]
    fn the_key_is_put_back_when_the_call_is_made() {
        let Ok(key) = std::env::var("VOX_ROOM_MCP_KEY") else {
            panic!("set VOX_ROOM_MCP_KEY to an rdpk-… key");
        };
        let s = Server::new(Config {
            name: "art".into(),
            url: "https://mcp.retrodiffusion.ai/mcp".into(),
            key,
            only: vec!["get_balance".into()],
        });
        s.connect().expect("say hello");
        let stash = Stash::default();
        let said = s
            .call("Vox", "art_get_balance", &json!({}), &stash)
            .expect("ask what the balance is");
        println!("{said}");
        assert!(!said.is_empty());
        // It said something rather than making something: nothing for
        // `set_avatar` to be handed.
        assert_eq!(stash.held("Vox"), 0);
    }

    #[test]
    fn base64_is_read_in_either_alphabet_and_out_of_a_data_uri() {
        assert_eq!(decode("aGk=").unwrap(), b"hi");
        assert_eq!(decode("data:image/png;base64,aGk=").unwrap(), b"hi");
        assert!(decode("not base64 at all !!").is_err());
    }
}
