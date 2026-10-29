//! The AI in the room: asks an OpenAI-compatible chat endpoint for a
//! reply, splits the stream into sentences, has each one synthesized, and
//! hands the audio to the room, which plays it and decides what was said.
//!
//! Two threads, so the model keeps streaming while a sentence is being
//! synthesized: the model thread turns a [`Turn`] into sentences, and the
//! voice thread turns sentences into audio. Both drop a reply as soon as
//! the room's generation counter moves past it (someone talked over it,
//! or a new turn began).

use std::io::BufRead as _;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context as _, Result};
use crossbeam_channel::{Receiver, Sender};
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::room::{GameSettings, ReplyMode, Role, Room, Turn};
use crate::tts::Voice;

/// The default instructions; `{name}` and `{people}` are filled in.
pub const DEFAULT_PROMPT: &str = "\
You are {name}, one of the voices in a live voice chat with {people}. Each \
message from the people is a transcript, every line starting with the \
speaker's name, and it comes from speech-to-text, so expect recognition \
errors and read past them. Your replies are read aloud by a text-to-speech \
voice: talk conversationally, in plain spoken sentences, usually briefly, and \
address people by name when it helps. Never use markdown, lists, headings, \
code blocks, emoji, or URLs, and never start with your own name. If someone \
cut off your previous reply, don't repeat it unless asked.";

/// Added to the lobby's prompt: the AI can design games with the players.
const GAMES_NOTE: &str = "\
You can also design games with the players, to play here by voice. When they \
want one, talk it through with them, and use your own judgment generously for \
whatever they leave open: genre, rules, your role as game master, tone, \
pacing. Propose a name and check it is free with check_game_name. Only when \
the players have confirmed the details, write the game's prompt yourself and \
save it with save_game. The prompt becomes your complete instructions when the \
room enters the game, and you won't remember this conversation then, so put \
everything in it: premise, setting, rules, how to run turns among several \
players, and how to open. If the game has anything meant for one player \
alone, say in its prompt how to deal it with send_card. A game also sets \
reply_mode, which is how the room runs while it is played: \"always\" for a \
game you deal the turns in, where you answer every pause; \"named\" for one \
where you only speak when a player calls you; \"room\" (the default) to \
leave it however the people have the room set. Decide it from how the game \
actually plays and say which you picked when you propose it. The players never \
see the prompt, so never read it aloud or describe its secrets; a card with \
its name and summary appears in the chat. The players run everything by \
voice: when they ask to play a game, start it with start_game (that ends \
this conversation, so don't say more \
than a few words first). When they want a game changed, use read_game and \
revise_game. When they want one deleted, say which game you mean and call \
delete_game only after they confirm; it can't be undone. list_games shows \
the saved games; speech recognition mangles names, so match what you hear \
to the closest saved one, and ask if it's unclear. Whatever you say \
before calling a tool is spoken straight away, so after the tool answers, \
carry on without repeating it.";

/// Added to the prompt, in the lobby and in a game: the private cards.
const CARDS_NOTE: &str = "\
Everyone has a private area of their own screen that nobody else can see, and \
send_card puts a card in one person's. Use it for anything meant for one \
person alone: their secret role, their hand, what only their character \
notices, a private instruction. Say nothing aloud about what is on a card -- \
the whole room hears your voice -- but do say that you have sent one when \
they should go and look, without giving it away. list_people is who is in the \
room, with the exact name to address a card to and how many cards each is \
already holding; speech recognition mangles names, so match what you hear to \
the closest name on that list, and check it there rather than guessing. Cards \
stay until the person discards one or the conversation starts over.";

/// Added to the prompt, in the lobby and in a game: how the room decides
/// the AI's turn, and how it bows out.
const TURN_NOTE: &str = "\
You are answering because the room gave you the turn: either someone said \
your name, which puts you in a back-and-forth where you answer every pause \
without being named again, or the room is set to answer every pause anyway. \
Either way, when the people are done with you -- they thank you, say \
goodbye, say they will be back, or simply turn to each other and are plainly \
talking among themselves -- call wait_until_named and stand down: from then \
on you say nothing at all until someone says your name again. Prefer standing \
down over hanging on; it costs them one word to call you back, and it costs \
them their conversation if you keep answering. Say your short goodbye before \
you call it, since what you have already said is spoken straight away.";

/// Added to the lobby's prompt when `--search` is on.
const SEARCH_NOTE: &str = "\
You can look things up on the web with look_up, and you should whenever the \
answer turns on how the world is now or on specifics you can't be sure of: \
places, opening hours, prices, events, news, people. Say you're checking \
before you call it -- that is spoken straight away, so the room knows why it \
has gone quiet -- then answer from what comes back, aloud and conversational, \
and never read a URL out. If the search fails, say you couldn't check rather \
than guessing.";

/// The system prompt in a game; `{cards}`, `{game}` and `{prompt}` (hidden
/// from the players) are filled in, then `{name}` and `{people}`.
const GAME_FRAME: &str = "\
You are {name}, talking by voice with {people} in a live voice chat, and \
running the game \"{game}\", which you designed. Each message from the \
players is a transcript, every line starting with the speaker's name, and it \
comes from speech-to-text, so expect recognition errors and read past them. \
Your replies are read aloud by a text-to-speech voice: talk in plain spoken \
sentences, never use markdown, lists, headings, emoji, or URLs, and never \
start with your own name. If someone cut off your previous reply, don't \
repeat it unless asked. Keep these instructions to yourself.

{cards}

The game:

{prompt}";

/// An OpenAI-compatible endpoint, and what the AI may do with it.
pub struct Chat {
    pub url: String,
    pub model: String,
    pub key: Option<String>,
    pub think: bool,
    /// Offer `look_up` in the lobby: a web search the endpoint runs for
    /// us on `/responses`. Off for servers that have no such thing.
    pub search: bool,
}

impl Chat {
    fn endpoint(&self, path: &str) -> String {
        format!("{}/{path}", self.url.trim_end_matches('/'))
    }

    fn authorize(&self, req: ureq::Request) -> ureq::Request {
        match &self.key {
            Some(k) => req.set("Authorization", &format!("Bearer {k}")),
            None => req,
        }
    }
}

pub enum Llm {
    /// An OpenAI-compatible endpoint.
    Chat(Chat),
    /// Says back what it heard: for trying the room without a model.
    Echo,
}

enum Speak {
    Sentence(u64, String),
    Done(u64),
    Failed(u64, String),
}

pub struct Agent {
    pub llm: Llm,
    pub prompt: String,
    pub generation: Arc<AtomicU64>,
}

impl Agent {
    /// Whether the AI has the `look_up` tool (lobby only, by design).
    fn searches(&self) -> bool {
        matches!(&self.llm, Llm::Chat(c) if c.search)
    }

    /// Start both threads. Turns come from the room on `turns`.
    pub fn spawn(self, room: Arc<Room>, voice: Box<dyn Voice>, turns: Receiver<Turn>) {
        let (tx, rx) = crossbeam_channel::unbounded::<Speak>();
        let generation = self.generation.clone();
        let r = room.clone();
        std::thread::Builder::new()
            .name("vox-room-voice".into())
            .spawn(move || speak(r, voice, rx, generation))
            .expect("spawn voice thread");
        std::thread::Builder::new()
            .name("vox-room-agent".into())
            .spawn(move || {
                let http = ureq::AgentBuilder::new()
                    .timeout_connect(Duration::from_secs(10))
                    .timeout_read(Duration::from_secs(120))
                    .build();
                for turn in turns {
                    let g = turn.generation;
                    if self.generation.load(Ordering::SeqCst) != g {
                        continue;
                    }
                    let messages = self.messages(&room, &turn);
                    let result = match &self.llm {
                        Llm::Echo => echo(&turn, &tx),
                        Llm::Chat(chat) => stream_reply(
                            &http,
                            &room,
                            chat,
                            &messages,
                            turn.game.is_none(),
                            g,
                            turn.asked,
                            &self.generation,
                            &tx,
                        ),
                    };
                    let _ = tx.send(match result {
                        Ok(()) => Speak::Done(g),
                        Err(e) => Speak::Failed(g, format!("{e:#}")),
                    });
                }
            })
            .expect("spawn agent thread");
    }

    fn messages(&self, room: &Room, turn: &Turn) -> Vec<Value> {
        let cfg = room.config();
        // Who is here, and anyone who spoke in the history.
        let mut people: Vec<String> = turn
            .messages
            .iter()
            .filter(|(r, _)| *r == Role::User)
            .flat_map(|(_, t)| {
                t.lines()
                    .filter_map(|l| l.split_once(": ").map(|(w, _)| w.trim().to_string()))
                    .filter(|w| !w.is_empty() && !w.contains(char::is_whitespace))
            })
            .chain(turn.people.iter().cloned())
            .collect();
        people.sort();
        people.dedup();
        let people = if people.is_empty() {
            "the people here".to_string()
        } else {
            people.join(" and ")
        };
        let search = if self.searches() {
            format!("\n\n{SEARCH_NOTE}")
        } else {
            String::new()
        };
        let system = match &turn.game {
            None => format!(
                "{}\n\n{GAMES_NOTE}\n\n{CARDS_NOTE}\n\n{TURN_NOTE}{search}",
                self.prompt
            ),
            Some((game, prompt)) => game_prompt(game, prompt),
        }
        .replace("{name}", &cfg.agent_name)
        .replace("{people}", &people);
        let mut out = vec![json!({ "role": "system", "content": system })];
        for (role, text) in &turn.messages {
            let role = match role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            out.push(json!({ "role": role, "content": text }));
        }
        out
    }
}

/// The whole system prompt for a game: the frame, the notes the AI has
/// wherever it is, and the game's own hidden instructions.
fn game_prompt(game: &str, prompt: &str) -> String {
    GAME_FRAME
        .replace("{cards}", &format!("{CARDS_NOTE}\n\n{TURN_NOTE}"))
        .replace("{game}", game)
        .replace("{prompt}", prompt)
}

/// The voice thread: synthesize each sentence and hand it to the room.
fn speak(
    room: Arc<Room>,
    mut voice: Box<dyn Voice>,
    rx: Receiver<Speak>,
    generation: Arc<AtomicU64>,
) {
    // A reply whose voice failed: the rest of it is skipped.
    let mut failed = None;
    for job in rx {
        match job {
            Speak::Sentence(g, text) => {
                if generation.load(Ordering::SeqCst) != g || failed == Some(g) {
                    continue;
                }
                let started = Instant::now();
                match voice.speak(&text) {
                    Ok(samples) => {
                        info!(
                            g,
                            chars = text.len(),
                            ms = started.elapsed().as_millis() as u64,
                            audio_ms = samples.len() as u64 * 1000 / crate::mixer::RATE as u64,
                            "reply: sentence synthesized"
                        );
                        if !room.agent_speak(g, &text, samples) {
                            debug!(g, "reply dropped while it was synthesized");
                        }
                    }
                    Err(e) => {
                        room.agent_failed(g, &format!("the voice failed on {text:?}: {e:#}"));
                        failed = Some(g);
                    }
                }
            }
            Speak::Done(g) if failed != Some(g) => room.agent_done(g),
            Speak::Done(_) => {}
            Speak::Failed(g, e) => room.agent_failed(g, &e),
        }
    }
}

fn echo(turn: &Turn, tx: &Sender<Speak>) -> Result<()> {
    let last = turn
        .messages
        .iter()
        .rev()
        .find(|(r, _)| *r == Role::User)
        .and_then(|(_, t)| t.lines().last())
        .unwrap_or("nothing");
    let (who, what) = last.split_once(':').unwrap_or(("someone", last));
    let _ = tx.send(Speak::Sentence(
        turn.generation,
        format!("{} said {}", who.trim(), what.trim()),
    ));
    let _ = tx.send(Speak::Sentence(
        turn.generation,
        "That is all I heard.".into(),
    ));
    Ok(())
}

/// Rounds of tool calls in one reply before the model must just talk.
const TOOL_ROUNDS: usize = 4;

/// The endpoint only takes tools with reasoning off (OpenAI's reasoning
/// models on /chat/completions): learned once, then sent every time.
static REASONING_OFF: AtomicBool = AtomicBool::new(false);
/// The endpoint refused tools and the room has been told.
static TOOLS_REFUSED: AtomicBool = AtomicBool::new(false);

/// The model, one reply: stream it into sentences, and in the lobby run
/// the tools it calls and let it carry on with their results.
#[allow(clippy::too_many_arguments)]
fn stream_reply(
    http: &ureq::Agent,
    room: &Room,
    cfg: &Chat,
    messages: &[Value],
    lobby: bool,
    g: u64,
    asked: Instant,
    generation: &AtomicU64,
    tx: &Sender<Speak>,
) -> Result<()> {
    let (model, think) = (cfg.model.as_str(), cfg.think);
    let chat = cfg.endpoint("chat/completions");
    let mut messages = messages.to_vec();
    let mut kwargs = true;
    // The cards are offered in a game too; the game tools are the lobby's.
    let mut tools = true;
    let mut first = true;
    // What this reply has said, across tool rounds.
    let mut spoken: Vec<Vec<String>> = Vec::new();
    for round in 0..=TOOL_ROUNDS {
        let resp = loop {
            let req = cfg.authorize(http.post(&chat));
            let mut body = json!({ "model": model, "messages": messages, "stream": true });
            if kwargs {
                body["chat_template_kwargs"] = json!({ "enable_thinking": think });
            }
            if tools && round < TOOL_ROUNDS {
                body["tools"] = tools_for(lobby, cfg.search);
                if REASONING_OFF.load(Ordering::Relaxed) {
                    body["reasoning_effort"] = json!("none");
                }
            }
            match req.send_json(body) {
                Ok(r) => break r,
                Err(ureq::Error::Status(code, r)) => {
                    let detail = r.into_string().unwrap_or_default();
                    if code == 400 && kwargs && detail.contains("chat_template_kwargs") {
                        kwargs = false;
                        continue;
                    }
                    if code == 400
                        && tools
                        && !think
                        && detail.contains("reasoning_effort")
                        && !REASONING_OFF.swap(true, Ordering::Relaxed)
                    {
                        info!("{chat} only takes tools without reasoning; turning it off");
                        continue;
                    }
                    if code == 400 && tools && detail.contains("tool") {
                        warn!("{chat} refuses tools, so games and cards won't work: {detail}");
                        if !TOOLS_REFUSED.swap(true, Ordering::Relaxed) {
                            room.error(&format!(
                                "{model} won't take tools here, so games and cards won't work: {}",
                                error_message(&detail)
                            ));
                        }
                        tools = false;
                        continue;
                    }
                    bail!(
                        "{chat}: HTTP {code}: {}",
                        detail.chars().take(300).collect::<String>()
                    );
                }
                Err(e) => return Err(e).with_context(|| format!("POST {chat}")),
            }
        };
        let ms = || asked.elapsed().as_millis() as u64;
        info!(g, round, after_asking_ms = ms(), "reply: stream opened");
        let mut pending = String::new();
        let mut said = String::new();
        let mut calls: Vec<ToolCall> = Vec::new();
        for line in std::io::BufReader::new(resp.into_reader()).lines() {
            if generation.load(Ordering::SeqCst) != g {
                info!(g, "reply dropped");
                return Ok(());
            }
            let line = line.context("read reply stream")?;
            let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                continue;
            };
            if data == "[DONE]" {
                break;
            }
            let Ok(v) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            let delta = &v["choices"][0]["delta"];
            if let Some(piece) = delta["content"].as_str() {
                if said.is_empty() && first && !piece.trim().is_empty() {
                    info!(g, after_asking_ms = ms(), "reply: first words");
                }
                pending.push_str(piece);
                said.push_str(piece);
            }
            if let Some(parts) = delta["tool_calls"].as_array() {
                for part in parts {
                    let i = part["index"].as_u64().unwrap_or(0) as usize;
                    while calls.len() <= i {
                        calls.push(ToolCall::default());
                    }
                    let c = &mut calls[i];
                    if let Some(id) = part["id"].as_str() {
                        c.id.push_str(id);
                    }
                    if let Some(n) = part["function"]["name"].as_str() {
                        c.name.push_str(n);
                    }
                    if let Some(a) = part["function"]["arguments"].as_str() {
                        c.arguments.push_str(a);
                    }
                }
            }
            while let Some(end) =
                sentence_end(&pending).or_else(|| first.then(|| clause_end(&pending)).flatten())
            {
                let s: String = pending.drain(..end).collect();
                if first {
                    info!(
                        g,
                        after_asking_ms = ms(),
                        chars = s.len(),
                        "reply: first sentence"
                    );
                    first = false;
                }
                send_sentence(tx, g, &s, &mut spoken);
            }
        }
        send_sentence(tx, g, &pending, &mut spoken);
        if !pending.trim().is_empty() {
            first = false;
        }
        calls.retain(|c| !c.name.is_empty());
        if calls.is_empty() {
            return Ok(());
        }
        // Having answered after one tool call, a model that calls another
        // tends to start its answer over in new words: run the calls (a
        // save still makes its card) but let this reply end here.
        let answered = round > 0 && !said.trim().is_empty();
        // The model's text so far stays in its turn, with the calls.
        messages.push(json!({
            "role": "assistant",
            "content": if said.trim().is_empty() { Value::Null } else { Value::from(said) },
            "tool_calls": calls.iter().enumerate().map(|(i, c)| json!({
                "id": c.id(i),
                "type": "function",
                "function": { "name": c.name, "arguments": c.arguments },
            })).collect::<Vec<_>>(),
        }));
        let spoke = !spoken.is_empty();
        for (i, c) in calls.iter().enumerate() {
            let mut result = run_tool(room, cfg, g, c);
            info!(g, tool = c.name, %result, "reply: tool");
            if answered {
                continue;
            }
            if spoke {
                result.push_str(
                    "\n(What you said before this call has already been spoken aloud. \
                     Carry on from there; don't repeat it.)",
                );
            }
            messages.push(json!({ "role": "tool", "tool_call_id": c.id(i), "content": result }));
        }
        if room.is_starting(g) {
            info!(g, "reply: started a game, so it ends here");
            return Ok(());
        }
        if generation.load(Ordering::SeqCst) != g {
            info!(g, "reply dropped");
            return Ok(());
        }
        if answered {
            info!(
                g,
                "reply: already answered, so not asking again after the tools"
            );
            return Ok(());
        }
    }
    Ok(())
}

#[derive(Default, Debug)]
struct ToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl ToolCall {
    fn id(&self, i: usize) -> String {
        if self.id.is_empty() {
            format!("call_{i}")
        } else {
            self.id.clone()
        }
    }
}

/// The `error.message` of an OpenAI-style error body, else the body.
fn error_message(detail: &str) -> String {
    serde_json::from_str::<Value>(detail)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| detail.chars().take(300).collect())
}

/// How long to wait for a web search before giving up on it. Long enough
/// for a real search, short enough that the room isn't silent for ever.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(30);
/// Cap the answer: it gets read aloud, and the model has to speak it.
const SEARCH_TOKENS: u32 = 700;

/// A function tool, as the endpoint wants it.
fn tool(n: &str, d: &str, props: Value, required: &[&str]) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": n,
            "description": d,
            "parameters": { "type": "object", "properties": props, "required": required },
        }
    })
}

/// The tools the model has everywhere: who is here, the cards it can deal
/// them one at a time, and standing down.
fn room_tools() -> Vec<Value> {
    vec![
        tool(
            "list_people",
            "List everyone in the room: their exact name, whether they have the \
             room open, whether their microphone is open, and how many cards \
             they are holding. Use it to get a name right before sending a \
             card.",
            json!({}),
            &[],
        ),
        tool(
            "send_card",
            "Put a card in one person's private area of the screen, which only \
             they can see. Use it for anything meant for them alone -- a secret \
             role, a hand, what only their character notices, a private \
             instruction -- and never say aloud what is on it. One person per \
             card: to tell two people different things, send two cards.",
            json!({
                "to": { "type": "string", "description": "Exactly the name list_people gives for that person." },
                "title": { "type": "string", "description": "A few words naming the card, e.g. \"Your role\" or \"What you notice\"." },
                "body": { "type": "string", "description": "What that person alone is to read. Written to be read, not spoken." },
            }),
            &["to", "title", "body"],
        ),
        tool(
            "wait_until_named",
            "Stop answering, and say nothing at all until someone says your \
             name again. Call it when the people are done talking to you -- \
             they thank you, say goodbye, or turn to talking among themselves \
             -- so that you don't answer a conversation that isn't yours.",
            json!({}),
            &[],
        ),
    ]
}

/// The `reply_mode` a game is saved with.
fn reply_mode_arg() -> Value {
    json!({
        "type": "string",
        "enum": ["room", "always", "named"],
        "description": "How the room runs while this game is played: \"always\" (you answer every pause, for a game you deal the turns in), \"named\" (you speak only when a player says your name), or \"room\" (leave it however the people have it set). Defaults to \"room\".",
    })
}

/// Everything the model may do this turn: the cards anywhere, and in the
/// lobby designing games and looking things up when `search` is on.
fn tools_for(lobby: bool, search: bool) -> Value {
    let name =
        json!({ "type": "string", "description": "The game's name, as the players will see it." });
    if !lobby {
        return Value::Array(room_tools());
    }
    let game = json!({
        "name": name,
        "summary": { "type": "string", "description": "One or two sentences the players see on the game's button." },
        "prompt": { "type": "string", "description": "Your complete hidden instructions for running the game." },
        "reply_mode": reply_mode_arg(),
    });
    let mut revise = game.clone();
    revise["new_name"] =
        json!({ "type": "string", "description": "A new name, if the game is being renamed." });
    let mut tools = json!([
        tool("list_games", "List the saved games: name, summary, and whether they have been played (and so are fixed).", json!({}), &[]),
        tool("check_game_name", "Check whether a name is free for a new game.", json!({ "name": name }), &["name"]),
        tool("save_game", "Save a new game once the players have confirmed it. Its button appears in the chat.", game, &["name", "summary", "prompt"]),
        tool("read_game", "Read the summary and hidden prompt of a saved game, to revise it.", json!({ "name": name }), &["name"]),
        tool("revise_game", "Replace a saved game with a revised version; its card in the chat is replaced by a new one.", revise, &["name", "summary", "prompt"]),
        tool("start_game", "Start playing a saved game: everyone moves into a fresh conversation run by its prompt.", json!({ "name": name }), &["name"]),
        tool("delete_game", "Delete a saved game for good, once the players have confirmed which one.", json!({ "name": name }), &["name"]),
    ]);
    tools
        .as_array_mut()
        .expect("tools is an array")
        .extend(room_tools());
    if search {
        tools.as_array_mut().expect("tools is an array").push(tool(
            "look_up",
            "Search the web and answer one question from what it finds. Use it \
             whenever the answer depends on the world as it is now or on \
             specifics you can't be sure of -- places, opening hours, prices, \
             events, news, people, anything recent. Prefer it over guessing. \
             Ask one plain question, with the place or date in it.",
            json!({ "question": {
                "type": "string",
                "description": "The question, in full, as you would ask a person: \"good restaurants in Boulder, Colorado\".",
            }}),
            &["question"],
        ));
    }
    tools
}

/// Answer one question with a web search, letting the endpoint do the
/// searching (OpenAI's `/responses` with its hosted `web_search` tool).
/// Not streamed: the model is waiting on the result, and the room shows
/// it as thinking until it comes back.
fn look_up(cfg: &Chat, question: &str) -> Result<String> {
    // Not the shared agent: that one waits 120s for the model to stream,
    // and a search that hung for 120s would leave the room silent.
    let http = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout(SEARCH_TIMEOUT)
        .build();
    let url = cfg.endpoint("responses");
    let body = json!({
        "model": cfg.model,
        "input": question,
        "tools": [{ "type": "web_search" }],
        "max_output_tokens": SEARCH_TOKENS,
    });
    let resp = match cfg.authorize(http.post(&url)).send_json(body) {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let detail = r.into_string().unwrap_or_default();
            bail!(
                "{url}: HTTP {code}: {}",
                detail.chars().take(200).collect::<String>()
            );
        }
        Err(e) => return Err(e).with_context(|| format!("POST {url}")),
    };
    let v: Value = resp.into_json().context("read the search result")?;
    let text = search_text(&v);
    if text.trim().is_empty() {
        bail!("the search came back empty");
    }
    Ok(text)
}

/// The answer out of a `/responses` body: the `output_text` of its
/// message items, and nothing else.
///
/// `/responses` returns a list of output items -- `reasoning`,
/// `web_search_call`, `message`. Only the message is the answer. The
/// reasoning item carries a `content` array of its own, empty in
/// practice but not by contract, and whatever the model thought on its
/// way to an answer must never reach the voice.
fn search_text(v: &Value) -> String {
    v["output"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|i| i["type"] == "message")
                .flat_map(|i| i["content"].as_array().into_iter().flatten())
                .filter(|c| c["type"] == "output_text")
                .filter_map(|c| c["text"].as_str())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

/// Run one tool call; the result is JSON for the model.
fn run_tool(room: &Room, cfg: &Chat, g: u64, call: &ToolCall) -> String {
    let args: Value = serde_json::from_str(&call.arguments).unwrap_or(Value::Null);
    let arg = |k: &str| args[k].as_str().unwrap_or("").to_string();
    // An unknown reply_mode would quietly become the default, so say so.
    let settings = || match args["reply_mode"].as_str() {
        None => Ok(GameSettings::default()),
        Some(m) => match ReplyMode::parse(m) {
            Some(reply) => Ok(GameSettings { reply }),
            None => Err(format!(
                "reply_mode is \"room\", \"always\" or \"named\", not {m:?}"
            )),
        },
    };
    let result = match call.name.as_str() {
        "list_games" => Ok(json!({ "games": room.list_games() })),
        "check_game_name" => room
            .check_game_name(&arg("name"))
            .map(|()| json!({ "free": true })),
        "save_game" => settings().and_then(|s| {
            room.save_game(g, &arg("name"), &arg("summary"), &arg("prompt"), s)
                .map(|()| json!({ "saved": arg("name").trim() }))
        }),
        "read_game" => room.read_game(&arg("name")).map(|(summary, s, prompt)| {
            json!({ "summary": summary, "reply_mode": s.reply.as_str(), "prompt": prompt })
        }),
        "revise_game" => settings().and_then(|s| {
            room.revise_game(
                g,
                &arg("name"),
                args["new_name"].as_str(),
                &arg("summary"),
                &arg("prompt"),
                s,
            )
            .map(|name| json!({ "saved": name }))
        }),
        "start_game" => room
            .start_game(g, &arg("name"))
            .map(|name| json!({ "started": name })),
        "delete_game" => room
            .remove_game(g, &arg("name"))
            .map(|name| json!({ "deleted": name })),
        "list_people" => Ok(json!({ "people": room.people_here() })),
        "wait_until_named" => {
            // What the room is doing afterwards: a game that answers at
            // every pause still does, and the model should know.
            let mode = room.wait_until_named();
            Ok(json!({ "answering": mode }))
        }
        "send_card" => room
            .send_card(&arg("to"), &arg("title"), &arg("body"))
            .map(|to| json!({ "sent": true, "to": to })),
        "look_up" => {
            let question = arg("question");
            info!(g, %question, "reply: looking it up");
            match look_up(cfg, &question) {
                Ok(answer) => Ok(json!({ "answer": answer })),
                Err(err) => {
                    // The model is told, so it can say it couldn't check
                    // rather than inventing an answer.
                    warn!(g, "look_up failed: {err:#}");
                    Err(format!(
                        "the search failed ({err}); say you couldn't look it up, and don't guess"
                    ))
                }
            }
        }
        other => Err(format!("there is no tool called {other:?}")),
    };
    match result {
        Ok(v) => v.to_string(),
        Err(e) => json!({ "error": e }).to_string(),
    }
}

/// Hand a sentence to the voice, unless it repeats one this reply has
/// already said (models tend to start over after a tool call).
fn send_sentence(tx: &Sender<Speak>, g: u64, s: &str, spoken: &mut Vec<Vec<String>>) {
    let s = clean(s);
    if s.is_empty() {
        return;
    }
    let w = sentence_words(&s);
    if spoken.iter().any(|old| repeats(old, &w)) {
        info!(g, sentence = %s, "reply: dropped a repeated sentence");
        return;
    }
    spoken.push(w);
    let _ = tx.send(Speak::Sentence(g, s));
}

fn sentence_words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Shortest sentence that counts as a repeat: short ones ("Sure.") recur
/// for real.
const REPEAT_MIN_WORDS: usize = 5;
/// A shorter one said again word for word ("Quick Draw is available.").
const EXACT_REPEAT_MIN_WORDS: usize = 3;

/// `b` says nearly the same as `a`: most of the longer one's words shared.
fn repeats(a: &[String], b: &[String]) -> bool {
    if a == b && a.len() >= EXACT_REPEAT_MIN_WORDS {
        return true;
    }
    if a.len().min(b.len()) < REPEAT_MIN_WORDS {
        return false;
    }
    let mut rest: Vec<&String> = a.iter().collect();
    let mut shared = 0;
    for w in b {
        if let Some(i) = rest.iter().position(|r| *r == w) {
            rest.swap_remove(i);
            shared += 1;
        }
    }
    shared * 4 >= a.len().max(b.len()) * 3
}

/// Strip markdown that shouldn't be read aloud.
fn clean(s: &str) -> String {
    s.replace(['*', '#', '`'], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Byte length of the first whole sentence in `text`: up to a `.`, `?`,
/// `!` (and closing quotes) followed by whitespace, or a line break.
/// (The same rule as the `agent` responder.)
fn sentence_end(text: &str) -> Option<usize> {
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '\n' && !text[..i].trim().is_empty() {
            return Some(i + 1);
        }
        if matches!(c, '.' | '?' | '!') {
            let mut end = i + c.len_utf8();
            while let Some(&(j, q)) = chars.peek() {
                if matches!(q, '"' | '\'' | ')' | '”' | '’' | '.' | '?' | '!') {
                    end = j + q.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
            if text[end..].starts_with(char::is_whitespace) {
                return Some(end);
            }
        }
    }
    None
}

/// Words a clause needs before it is worth speaking on its own.
const CLAUSE_WORDS: usize = 4;

/// Byte length of the first clause in `text`: up to a `,`, `;`, `:` or
/// dash followed by whitespace, once there are a few words before it.
fn clause_end(text: &str) -> Option<usize> {
    let mut words = 0;
    let mut in_word = false;
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            in_word = false;
            continue;
        }
        if !in_word {
            in_word = true;
            words += 1;
        }
        if matches!(c, ',' | ';' | ':' | '—' | '–') && words >= CLAUSE_WORDS {
            let end = i + c.len_utf8();
            if text[end..].starts_with(char::is_whitespace) {
                return Some(end);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_sentence_ends() {
        assert_eq!(sentence_end("Hello there. How"), Some(12));
        assert_eq!(sentence_end("Wait... what"), Some(7));
        assert_eq!(sentence_end("No end yet."), None);
    }

    #[test]
    fn finds_clause_ends() {
        assert_eq!(clause_end("Well, that is a fine question, and"), Some(30));
        assert_eq!(clause_end("Well, sure"), None);
        assert_eq!(clause_end("It costs 1,000 coins, or"), Some(21));
    }

    #[test]
    fn drops_a_sentence_said_again_after_a_tool_call() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut spoken = Vec::new();
        for s in [
            "Sure.",
            "One idea is Quick Draw: I give you a category, and we take turns naming something.",
            "Sure.",
            "One idea is Quick Draw: I give you a category and we take turns naming things.",
            "Does that sound fun, or would you rather play something else?",
            "Quick Draw is available.",
            "Quick Draw is available.",
        ] {
            send_sentence(&tx, 1, s, &mut spoken);
        }
        drop(tx);
        let said: Vec<String> = rx
            .iter()
            .filter_map(|m| match m {
                Speak::Sentence(_, s) => Some(s),
                _ => None,
            })
            .collect();
        assert_eq!(said.len(), 5, "{said:?}");
        assert_eq!(said[2], "Sure.", "short sentences may recur");
    }

    fn tool_names(lobby: bool, search: bool) -> Vec<String> {
        tools_for(lobby, search)
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn look_up_is_offered_only_when_search_is_on() {
        let without = tool_names(true, false);
        assert!(without.contains(&"save_game".to_string()));
        assert!(!without.contains(&"look_up".to_string()));

        let with = tool_names(true, true);
        // The game and card tools are untouched; look_up is added to them.
        assert_eq!(with.len(), without.len() + 1);
        assert_eq!(&with[..without.len()], &without[..]);
        assert_eq!(with.last().unwrap(), "look_up");
    }

    #[test]
    fn the_room_tools_are_offered_in_a_game_and_the_game_tools_are_not() {
        let in_game = tool_names(false, true);
        assert_eq!(in_game, ["list_people", "send_card", "wait_until_named"]);
        // In the lobby they are there as well, alongside the games.
        let lobby = tool_names(true, false);
        for t in &in_game {
            assert!(lobby.contains(t), "{t} is missing from the lobby");
        }
        assert!(lobby.contains(&"save_game".to_string()));
    }

    #[test]
    fn a_game_is_told_about_the_cards_without_being_told_about_games() {
        let frame = game_prompt("Dragon Heist", "Run a heist.");
        assert!(frame.contains("send_card"));
        assert!(frame.contains("list_people"));
        assert!(frame.contains("wait_until_named"));
        assert!(!frame.contains("{cards}"));
        assert!(!frame.contains("save_game"));
    }

    /// The shape a real /responses search comes back in (field names and
    /// item order captured from the live endpoint).
    #[test]
    fn a_search_answer_is_the_message_and_never_the_reasoning() {
        let body = json!({
            "status": "completed",
            "output": [
                {
                    "type": "reasoning",
                    "summary": [],
                    // Empty from the live endpoint, but not by contract:
                    // if it ever fills, it must not be spoken.
                    "content": [{ "type": "reasoning_text", "text": "they probably mean the city" }],
                },
                { "type": "web_search_call", "status": "completed" },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "Gaia Masala and Burger is well liked in Boulder.",
                        "annotations": [{ "type": "url_citation", "url": "https://example.com" }],
                    }],
                },
            ],
        });
        assert_eq!(
            search_text(&body),
            "Gaia Masala and Burger is well liked in Boulder."
        );
        // Nothing to say is an error upstream, not an empty reply.
        assert_eq!(search_text(&json!({ "output": [] })), "");
        assert_eq!(search_text(&json!({ "error": { "message": "nope" } })), "");
    }

    #[test]
    fn cleans_markdown() {
        assert_eq!(clean("  **Sure**,  here's   `one`. "), "Sure, here's one.");
    }
}
