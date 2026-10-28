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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use crossbeam_channel::{Receiver, Sender};
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::room::{Role, Room, Turn};
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

pub enum Llm {
    /// An OpenAI-compatible endpoint.
    Chat {
        url: String,
        model: String,
        key: Option<String>,
        think: bool,
    },
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
                        Llm::Chat {
                            url,
                            model,
                            key,
                            think,
                        } => stream_reply(
                            &http,
                            url,
                            model,
                            key.as_deref(),
                            *think,
                            &messages,
                            g,
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
        let mut people: Vec<String> = turn
            .messages
            .iter()
            .filter(|(r, _)| *r == Role::User)
            .flat_map(|(_, t)| {
                t.lines()
                    .filter_map(|l| l.split_once(':').map(|(w, _)| w.trim().to_string()))
            })
            .collect();
        people.sort();
        people.dedup();
        let people = if people.is_empty() {
            "the people here".to_string()
        } else {
            people.join(" and ")
        };
        let system = self
            .prompt
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
                match voice.speak(&text) {
                    Ok(samples) => {
                        if !room.agent_speak(g, &text, samples) {
                            debug!(g, "reply dropped while it was synthesized");
                        }
                    }
                    Err(e) => {
                        warn!("voice failed: {e:#}");
                        room.agent_failed(g, &format!("the voice failed: {e:#}"));
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

#[allow(clippy::too_many_arguments)]
fn stream_reply(
    http: &ureq::Agent,
    url: &str,
    model: &str,
    key: Option<&str>,
    think: bool,
    messages: &[Value],
    g: u64,
    generation: &AtomicU64,
    tx: &Sender<Speak>,
) -> Result<()> {
    let chat = format!("{}/chat/completions", url.trim_end_matches('/'));
    let mut kwargs = true;
    let resp = loop {
        let mut req = http.post(&chat);
        if let Some(k) = key {
            req = req.set("Authorization", &format!("Bearer {k}"));
        }
        let mut body = json!({ "model": model, "messages": messages, "stream": true });
        if kwargs {
            body["chat_template_kwargs"] = json!({ "enable_thinking": think });
        }
        match req.send_json(body) {
            Ok(r) => break r,
            Err(ureq::Error::Status(code, r)) => {
                let detail = r.into_string().unwrap_or_default();
                if code == 400 && kwargs && detail.contains("chat_template_kwargs") {
                    kwargs = false;
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
    info!(g, "reply stream opened");
    let mut pending = String::new();
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
        if let Some(piece) = v["choices"][0]["delta"]["content"].as_str() {
            pending.push_str(piece);
        }
        while let Some(end) = sentence_end(&pending) {
            let s: String = pending.drain(..end).collect();
            send_sentence(tx, g, &s);
        }
    }
    send_sentence(tx, g, &pending);
    Ok(())
}

fn send_sentence(tx: &Sender<Speak>, g: u64, s: &str) {
    let s = clean(s);
    if !s.is_empty() {
        let _ = tx.send(Speak::Sentence(g, s));
    }
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
    fn cleans_markdown() {
        assert_eq!(clean("  **Sure**,  here's   `one`. "), "Sure, here's one.");
    }
}
