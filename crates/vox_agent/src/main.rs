//! `agent NAME`: the responder for `scribe --chat NAME`. Watches NAME.db
//! for what the user says, asks an OpenAI-compatible chat endpoint for a
//! reply (the system prompt plus the conversation so far), and streams
//! the reply back into the database a sentence at a time, so scribe can
//! start speaking before the reply is finished. If the user interrupts
//! or says something new, the reply in progress is dropped.

use std::io::BufRead as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Context as _, Result};
use clap::Parser;
use serde_json::{json, Value};
use tracing::{info, warn};
use vox_chat::{Db, Kind, Role, Row};

const DEFAULT_PROMPT: &str = "\
You are talking with the user by voice. Their messages come from \
speech-to-text, so expect recognition errors and read past them. Your replies \
are read aloud by a text-to-speech voice: answer conversationally, in plain \
spoken sentences, usually briefly. Never use markdown, lists, headings, code \
blocks, emoji, or URLs. If the user cut off your previous reply, don't repeat \
it unless asked.";

/// Shown to the model after a reply the user talked over.
const CUT_NOTE: &str = " [the user interrupted here]";

#[derive(Parser)]
#[command(name = "agent", version, about)]
struct Cli {
    /// The conversation: NAME.db, as given to `scribe --chat NAME`
    /// (`.db` is added if missing).
    name: PathBuf,
    /// System prompt. Default: a short one for spoken conversation.
    #[arg(long, env = "VOX_AGENT_PROMPT", conflicts_with = "prompt_file")]
    prompt: Option<String>,
    /// Read the system prompt from a file.
    #[arg(long, env = "VOX_AGENT_PROMPT_FILE")]
    prompt_file: Option<PathBuf>,
    /// OpenAI-compatible base URL ending in /v1 (a local llama.cpp,
    /// ollama or vLLM server, or https://api.openai.com/v1).
    #[arg(
        long,
        env = "VOX_AGENT_URL",
        default_value = "http://127.0.0.1:9931/v1"
    )]
    url: String,
    /// Model name. Falls back to VOX_SCRIBE_LLM_MODEL.
    #[arg(long, env = "VOX_AGENT_MODEL")]
    model: Option<String>,
    /// Earlier messages sent along with each new one.
    #[arg(long, default_value_t = 40)]
    history: usize,
}

struct Llm {
    url: String,
    model: String,
    key: Option<String>,
    http: ureq::Agent,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let prompt = match (&cli.prompt, &cli.prompt_file) {
        (Some(p), _) => p.clone(),
        (None, Some(f)) => {
            std::fs::read_to_string(f).with_context(|| format!("read {}", f.display()))?
        }
        (None, None) => DEFAULT_PROMPT.to_string(),
    };
    // OPENAI_API_KEY only goes to OpenAI, not to a local server.
    let key = env("VOX_AGENT_KEY").or_else(|| {
        cli.url
            .contains("api.openai.com")
            .then(|| env("OPENAI_API_KEY"))
            .flatten()
    });
    let llm = Llm {
        url: cli.url.clone(),
        model: cli
            .model
            .clone()
            .or_else(|| env("VOX_SCRIBE_LLM_MODEL"))
            .context("set --model, VOX_AGENT_MODEL or VOX_SCRIBE_LLM_MODEL")?,
        key,
        http: ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(120))
            .build(),
    };
    let path = vox_chat::path_for(&cli.name);
    let db = Db::open(&path)?;
    info!(db = %path.display(), url = %llm.url, model = %llm.model, "agent ready");
    serve(&db, &llm, &prompt, cli.history)
}

/// Answer each new thing the user says, forever.
fn serve(db: &Db, llm: &Llm, prompt: &str, history: usize) -> Result<()> {
    let mut last = db.last_id()?;
    loop {
        let rows = db.since(last)?;
        // Only the newest thing said gets a reply.
        let Some(say) = rows
            .iter()
            .rev()
            .find(|r| r.role == Role::User && r.kind == Kind::Say)
            .cloned()
        else {
            if let Some(r) = rows.last() {
                last = r.id;
            }
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        last = say.id;
        info!(id = say.id, text = %say.text, "user");
        let messages = conversation(prompt, &db.since(0)?, say.id, history);
        if let Err(e) = reply(db, llm, &messages, say.id) {
            warn!("reply failed: {e:#}");
            if !superseded(db, say.id)? {
                db.reply(say.id, "Sorry, I couldn't get an answer just now.", false)?;
            }
        }
    }
}

/// The chat messages for the model: the system prompt, then up to
/// `history` earlier messages and the user message `upto`.
fn conversation(prompt: &str, rows: &[Row], upto: i64, history: usize) -> Vec<Value> {
    // (role, reply_to, text, cut)
    let mut turns: Vec<(Role, Option<i64>, String, bool)> = Vec::new();
    for r in rows.iter().filter(|r| r.id <= upto) {
        let text = r.text.trim();
        match (r.role, r.kind) {
            (Role::User, Kind::Say) => turns.push((Role::User, None, text.into(), false)),
            (Role::User, Kind::Interrupt) => {
                if let Some(t) = turns
                    .iter_mut()
                    .rev()
                    .find(|t| t.0 == Role::Assistant && t.1 == r.reply_to)
                {
                    t.3 = true;
                }
            }
            (Role::Assistant, _) => match turns.last_mut() {
                Some(t) if t.0 == Role::Assistant && t.1 == r.reply_to => {
                    if !text.is_empty() {
                        t.2 = format!("{} {text}", t.2).trim().to_string();
                    }
                }
                _ => turns.push((Role::Assistant, r.reply_to, text.into(), false)),
            },
        }
    }
    turns.retain(|t| !t.2.is_empty());
    let skip = turns.len().saturating_sub(history + 1);
    let mut out = vec![json!({ "role": "system", "content": prompt })];
    for (role, _, text, cut) in turns.into_iter().skip(skip) {
        let (role, content) = match role {
            Role::User => ("user", text),
            Role::Assistant if cut => ("assistant", format!("{text}{CUT_NOTE}")),
            Role::Assistant => ("assistant", text),
        };
        out.push(json!({ "role": role, "content": content }));
    }
    out
}

/// The user interrupted the reply to `id`, or said something new.
fn superseded(db: &Db, id: i64) -> Result<bool> {
    Ok(db.since(id)?.iter().any(|r| r.role == Role::User))
}

/// Stream the model's answer to user message `id` into the database.
fn reply(db: &Db, llm: &Llm, messages: &[Value], id: i64) -> Result<()> {
    let url = format!("{}/chat/completions", llm.url.trim_end_matches('/'));
    let mut req = llm.http.post(&url);
    if let Some(key) = &llm.key {
        req = req.set("Authorization", &format!("Bearer {key}"));
    }
    let body = json!({ "model": llm.model, "messages": messages, "stream": true });
    let resp = match req.send_json(body) {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let body = r.into_string().unwrap_or_default();
            let detail = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(String::from))
                .unwrap_or(body);
            bail!("{url}: HTTP {code}: {detail}");
        }
        Err(e) => return Err(e).with_context(|| format!("POST {url}")),
    };
    let mut pending = String::new();
    let mut checked = Instant::now();
    for line in std::io::BufReader::new(resp.into_reader()).lines() {
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
        // Stop as soon as the user talks over the reply.
        if checked.elapsed() >= Duration::from_millis(100) {
            checked = Instant::now();
            if superseded(db, id)? {
                info!(id, "interrupted");
                return Ok(());
            }
        }
        while let Some(end) = sentence_end(&pending) {
            let sentence: String = pending.drain(..end).collect();
            if superseded(db, id)? {
                info!(id, "interrupted");
                return Ok(());
            }
            info!(id, text = %sentence.trim(), "reply");
            db.reply(id, sentence.trim(), true)?;
        }
    }
    if superseded(db, id)? {
        info!(id, "interrupted");
        return Ok(());
    }
    let rest = pending.trim();
    if !rest.is_empty() {
        info!(id, text = %rest, "reply");
    }
    // The last row says the reply is complete, even if it's empty.
    db.reply(id, rest, false)?;
    Ok(())
}

/// Byte length of the first whole sentence in `text`: up to a `.`, `?`,
/// `!` (and closing quotes) followed by whitespace, or a line break.
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

    fn row(id: i64, role: Role, kind: Kind, reply_to: Option<i64>, text: &str) -> Row {
        Row {
            id,
            role,
            kind,
            reply_to,
            text: text.into(),
            more: false,
            at: String::new(),
        }
    }

    #[test]
    fn finds_sentence_ends() {
        assert_eq!(sentence_end("Hello there. How"), Some(12));
        assert_eq!(sentence_end("Is it \"good?\" Yes"), Some(13));
        assert_eq!(sentence_end("See d.rymcg.tech now"), None);
        assert_eq!(sentence_end("Wait... what"), Some(7));
        assert_eq!(sentence_end("Line one\nnext"), Some(9));
        assert_eq!(sentence_end("No end yet."), None);
    }

    #[test]
    fn builds_the_conversation() {
        let rows = [
            row(1, Role::User, Kind::Say, None, "Hi."),
            row(2, Role::Assistant, Kind::Say, Some(1), "Hello."),
            row(3, Role::Assistant, Kind::Say, Some(1), "How can I"),
            row(4, Role::User, Kind::Interrupt, Some(1), ""),
            row(5, Role::User, Kind::Say, None, "Tell me a joke."),
            row(6, Role::User, Kind::Say, None, "Later."),
        ];
        let m = conversation("P", &rows, 5, 40);
        let pairs: Vec<(&str, &str)> = m
            .iter()
            .map(|v| (v["role"].as_str().unwrap(), v["content"].as_str().unwrap()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("system", "P"),
                ("user", "Hi."),
                ("assistant", "Hello. How can I [the user interrupted here]"),
                ("user", "Tell me a joke."),
            ]
        );
        // History keeps only the newest messages.
        assert_eq!(conversation("P", &rows, 5, 1).len(), 3);
    }
}
