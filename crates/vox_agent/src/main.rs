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
use tracing::{debug, info, warn};
use vox_chat::{Db, Kind, Role, Row};
use vox_transcribe::filters::sentence_end;

const DEFAULT_PROMPT: &str = "\
You are talking with the user by voice. Their messages come from \
speech-to-text, so expect recognition errors and read past them. Your replies \
are read aloud by a text-to-speech voice: answer conversationally, in plain \
spoken sentences, usually briefly. Never use markdown, lists, headings, code \
blocks, emoji, or URLs. If the user cut off your previous reply, don't repeat \
it unless asked.";

/// Shown to the model after a reply the user talked over.
const CUT_NOTE: &str = " [the user interrupted here]";

/// How long the startup check waits for the few words it asks for. Long
/// enough for a local server that loads the model on the first request.
const CHECK_TIMEOUT: Duration = Duration::from_secs(60);

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
    /// ollama or vLLM server, or https://api.openai.com/v1). The same
    /// service as scribe's --llm pass; the API key comes from
    /// VOX_SCRIBE_LLM_KEY (or OPENAI_API_KEY for OpenAI's URL).
    #[arg(
        long,
        env = "VOX_SCRIBE_LLM_URL",
        default_value = vox_transcribe::openai::DEFAULT_URL
    )]
    url: String,
    /// Model name.
    #[arg(long, env = "VOX_SCRIBE_LLM_MODEL")]
    model: Option<String>,
    /// Earlier messages sent along with each new one.
    #[arg(long, default_value_t = 40)]
    history: usize,
    /// Let a reasoning model think before it answers. Off by default:
    /// the thinking is never spoken, and on a local model it can take
    /// longer than the answer. (Sent as llama.cpp's
    /// `chat_template_kwargs.enable_thinking`; servers that reject it
    /// are retried without it.)
    #[arg(long, env = "VOX_AGENT_THINK")]
    think: bool,
}

struct Llm {
    /// Base URL ending in `/v1`.
    url: String,
    model: String,
    key: Option<String>,
    http: ureq::Agent,
    think: bool,
    /// The server rejected `chat_template_kwargs`; leave it out.
    no_kwargs: std::cell::Cell<bool>,
}

impl Llm {
    /// Where the chat completions are asked for.
    fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.url.trim_end_matches('/'))
    }

    /// A POST to `url`, with the API key if there is one.
    fn post(&self, url: &str) -> ureq::Request {
        let req = self.http.post(url);
        match &self.key {
            Some(key) => req.set("Authorization", &format!("Bearer {key}")),
            None => req,
        }
    }
}

/// The server's complaint, from an OpenAI-style `error.message` where it
/// sent one, else the body as it came.
fn refusal(resp: ureq::Response) -> String {
    let body = resp.into_string().unwrap_or_default();
    serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(String::from))
        .unwrap_or(body)
}

/// Whether a 400 is the server turning down the token limit we sent,
/// which is worth asking again without it.
fn limit_refused(detail: &str) -> bool {
    LIMITS.iter().any(|limit| detail.contains(limit))
}

/// Ways to cap the answer: reasoning models reject `max_tokens` and want
/// `max_completion_tokens`, older servers know only the first, and a few
/// take neither. Only the startup check sends one.
const LIMITS: [&str; 2] = ["max_tokens", "max_completion_tokens"];

/// Ask the model for a word before the conversation opens, so a wrong
/// URL, a missing key, a server that isn't running or a model it hasn't
/// got fails here — not in the middle of a conversation, where all the
/// user hears is "Sorry, I couldn't get an answer just now."
fn check(llm: &Llm) -> Result<()> {
    let url = llm.chat_url();
    let start = Instant::now();
    let mut limits = LIMITS.iter();
    let mut limit = limits.next();
    loop {
        let mut body = json!({
            "model": llm.model,
            "messages": [{ "role": "user", "content": "Say OK." }],
            "stream": false,
        });
        if let Some(limit) = limit {
            body[*limit] = json!(16);
        }
        match llm.post(&url).timeout(CHECK_TIMEOUT).send_json(body) {
            Ok(_) => {
                info!(
                    ms = start.elapsed().as_millis() as u64,
                    "the model answered"
                );
                return Ok(());
            }
            Err(ureq::Error::Status(code, resp)) => {
                let detail = refusal(resp);
                if code == 400 && limit.is_some() && limit_refused(&detail) {
                    debug!("server turned down {limit:?}; asking without it");
                    limit = limits.next();
                    continue;
                }
                bail!("{url}: HTTP {code}: {detail}");
            }
            Err(e) => return Err(e).with_context(|| format!("POST {url}")),
        }
    }
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
    let endpoint = vox_transcribe::openai::Endpoint::at(cli.url.clone());
    let llm = Llm {
        url: endpoint.url,
        model: cli
            .model
            .clone()
            .context("set --model or VOX_SCRIBE_LLM_MODEL")?,
        key: endpoint.api_key,
        http: ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(120))
            .build(),
        think: cli.think,
        no_kwargs: std::cell::Cell::new(false),
    };
    // Before anything else: a model that doesn't answer is no responder.
    check(&llm).with_context(|| {
        format!(
            "{} has nothing to say as {:?} (--url / VOX_SCRIBE_LLM_URL, \
             --model / VOX_SCRIBE_LLM_MODEL, VOX_SCRIBE_LLM_KEY)",
            llm.url, llm.model
        )
    })?;
    let path = vox_chat::path_for(&cli.name);
    let db = Db::open(&path)?;
    // scribe won't open a chat that nothing is answering.
    vox_chat::beat(&path, &format!("agent {}", std::process::id()))?;
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
        // Tell scribe it's being worked on (shown as thinking).
        db.reply(say.id, "", true)?;
        let messages = conversation(prompt, &db.rows_in(say.conversation)?, say.id, history);
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
    let url = llm.chat_url();
    let req = llm.post(&url);
    let chars: usize = messages
        .iter()
        .map(|m| m["content"].as_str().map_or(0, str::len))
        .sum();
    info!(
        id,
        messages = messages.len(),
        prompt_chars = chars,
        think = llm.think,
        "asking the model"
    );
    let start = Instant::now();
    let resp = loop {
        let mut body = json!({ "model": llm.model, "messages": messages, "stream": true });
        if !llm.no_kwargs.get() {
            body["chat_template_kwargs"] = json!({ "enable_thinking": llm.think });
        }
        match req.clone().send_json(body) {
            Ok(r) => break r,
            Err(ureq::Error::Status(code, resp)) => {
                let detail = refusal(resp);
                if code == 400 && !llm.no_kwargs.get() && detail.contains("chat_template_kwargs") {
                    warn!("server rejected chat_template_kwargs; retrying without it");
                    llm.no_kwargs.set(true);
                    continue;
                }
                bail!("{url}: HTTP {code}: {detail}");
            }
            Err(e) => return Err(e).with_context(|| format!("POST {url}")),
        }
    };
    info!(id, ms = start.elapsed().as_millis() as u64, "stream opened");
    let mut stats = Stats::default();
    let result = stream(db, id, resp, start, &mut stats);
    info!(
        id,
        total_ms = start.elapsed().as_millis() as u64,
        first_thought_ms = stats.first_thought,
        first_word_ms = stats.first_word,
        reasoning_chars = stats.reasoning,
        answer_chars = stats.answer,
        rows = stats.rows,
        finish = %stats.finish.as_deref().unwrap_or("-"),
        interrupted = stats.interrupted,
        "reply done"
    );
    if let Some(t) = &stats.timings {
        // llama.cpp's own numbers: prompt and generation speed.
        info!(
            id,
            cached = t["cache_n"].as_u64(),
            prompt_tokens = t["prompt_n"].as_u64(),
            prompt_ms = t["prompt_ms"].as_f64().map(|v| v as u64),
            generated_tokens = t["predicted_n"].as_u64(),
            tokens_per_s = t["predicted_per_second"]
                .as_f64()
                .map(|v| (v * 10.0).round() / 10.0),
            "server timings"
        );
    }
    if let Some(u) = &stats.usage {
        info!(
            id,
            prompt_tokens = u["prompt_tokens"].as_u64(),
            completion_tokens = u["completion_tokens"].as_u64(),
            "usage"
        );
    }
    result
}

/// What one reply took, for the log.
#[derive(Default)]
struct Stats {
    first_thought: Option<u64>,
    first_word: Option<u64>,
    reasoning: usize,
    answer: usize,
    rows: usize,
    finish: Option<String>,
    interrupted: bool,
    timings: Option<Value>,
    usage: Option<Value>,
}

/// Read the server-sent events of `resp` into rows for user message `id`.
fn stream(db: &Db, id: i64, resp: ureq::Response, start: Instant, st: &mut Stats) -> Result<()> {
    let ms = || start.elapsed().as_millis() as u64;
    let mut pending = String::new();
    let mut checked = Instant::now();
    let mut thinking_logged = Instant::now();
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
        let delta = &v["choices"][0]["delta"];
        if let Some(r) = delta["reasoning_content"]
            .as_str()
            .filter(|r| !r.is_empty())
        {
            if st.first_thought.is_none() {
                st.first_thought = Some(ms());
                info!(id, ms = ms(), "model is thinking (reasoning, not spoken)");
            }
            st.reasoning += r.len();
            if thinking_logged.elapsed() >= Duration::from_secs(5) {
                thinking_logged = Instant::now();
                info!(
                    id,
                    ms = ms(),
                    reasoning_chars = st.reasoning,
                    "still thinking"
                );
            }
            debug!(id, piece = r, "reasoning");
        }
        if let Some(piece) = delta["content"].as_str().filter(|p| !p.is_empty()) {
            if st.first_word.is_none() {
                st.first_word = Some(ms());
                info!(id, ms = ms(), "first words");
            }
            st.answer += piece.len();
            debug!(id, piece, "content");
            pending.push_str(piece);
        }
        if let Some(f) = v["choices"][0]["finish_reason"].as_str() {
            st.finish = Some(f.to_string());
        }
        if v["timings"].is_object() {
            st.timings = Some(v["timings"].clone());
        }
        if v["usage"].is_object() {
            st.usage = Some(v["usage"].clone());
        }
        // Stop as soon as the user talks over the reply.
        if checked.elapsed() >= Duration::from_millis(100) {
            checked = Instant::now();
            if superseded(db, id)? {
                st.interrupted = true;
                return Ok(());
            }
        }
        while let Some(end) = sentence_end(&pending) {
            let sentence: String = pending.drain(..end).collect();
            if superseded(db, id)? {
                st.interrupted = true;
                return Ok(());
            }
            info!(id, ms = ms(), text = %sentence.trim(), "reply");
            db.reply(id, sentence.trim(), true)?;
            st.rows += 1;
        }
    }
    if superseded(db, id)? {
        st.interrupted = true;
        return Ok(());
    }
    let rest = pending.trim();
    if !rest.is_empty() {
        info!(id, ms = ms(), text = %rest, "reply");
    }
    // The last row says the reply is complete, even if it's empty.
    db.reply(id, rest, false)?;
    st.rows += 1;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64, role: Role, kind: Kind, reply_to: Option<i64>, text: &str) -> Row {
        Row {
            id,
            conversation: 1,
            role,
            kind,
            reply_to,
            text: text.into(),
            more: false,
            at: String::new(),
        }
    }

    fn llm(url: &str) -> Llm {
        Llm {
            url: url.into(),
            model: "m".into(),
            key: None,
            http: ureq::AgentBuilder::new().build(),
            think: false,
            no_kwargs: std::cell::Cell::new(false),
        }
    }

    #[test]
    fn asks_one_path_whatever_the_url_ends_in() {
        assert_eq!(
            llm("http://h/v1").chat_url(),
            "http://h/v1/chat/completions"
        );
        assert_eq!(
            llm("http://h/v1/").chat_url(),
            "http://h/v1/chat/completions"
        );
    }

    #[test]
    fn knows_a_refused_token_limit_from_other_complaints() {
        assert!(limit_refused(
            "Unsupported parameter: 'max_tokens' is not supported with this model. \
             Use 'max_completion_tokens' instead."
        ));
        assert!(limit_refused("unknown field `max_completion_tokens`"));
        // Anything else is the server's real answer, not worth retrying.
        assert!(!limit_refused("context length exceeded"));
        assert!(!limit_refused(""));
    }

    #[test]
    fn reads_the_servers_complaint_out_of_its_body() {
        let json = r#"{"error":{"message":"nope","type":"invalid_request_error"}}"#;
        let resp = ureq::Response::new(400, "Bad Request", json).unwrap();
        assert_eq!(refusal(resp), "nope");
        // A server that sends something else is quoted as it is.
        let plain = ureq::Response::new(502, "Bad Gateway", "upstream died").unwrap();
        assert_eq!(refusal(plain), "upstream died");
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
