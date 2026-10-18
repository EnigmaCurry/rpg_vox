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
use vox_chat::{Db, Role, Turn};

const DEFAULT_PROMPT: &str = "\
You are talking with the user by voice. Their messages come from \
speech-to-text, so expect recognition errors and read past them. Your replies \
are read aloud by a text-to-speech voice: answer conversationally, in plain \
spoken sentences, usually briefly. Never use markdown, lists, headings, code \
blocks, emoji, or URLs. If the user talked over one of your replies, you are \
shown only the part they actually heard, marked as interrupted — carry on from \
there rather than repeating it.";

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
    // What we last took up. Whatever was said before we started is
    // only ours to answer if nobody has taken it up yet.
    let mut answered = match db.unanswered()? {
        Some(_) => 0,
        None => db.newest_user()?.map_or(0, |t| t.id),
    };
    loop {
        // The newest thing said, once they have finished saying it and
        // if it isn't the one we are already on. Asking which turn is
        // last in the file instead would miss a turn whenever a reply
        // we had begun landed after it.
        let Some(say) = db
            .newest_user()?
            .filter(|t| t.done && t.id != answered)
        else {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        answered = say.id;
        info!(id = say.id, text = %say.text, "user");
        // An empty turn: scribe shows it as thinking until words arrive.
        let reply = db.start(say.conversation, Role::Assistant, "", false)?;
        let messages = conversation(prompt, &db.turns_in(say.conversation)?, say.id, history);
        if let Err(e) = answer(db, llm, &messages, &say, reply) {
            warn!("reply failed: {e:#}");
            if !db.said_since(say.rev)? {
                db.extend(reply, "Sorry, I couldn't get an answer just now.", true)?;
            } else {
                db.extend(reply, "", true)?;
            }
        }
    }
}

/// The chat messages for the model: the system prompt, then up to
/// `history` earlier turns and the user turn `upto`.
///
/// A reply the user talked over goes in as the part they actually
/// heard ([`vox_chat::Turn::heard`]) — all they can be answering.
fn conversation(prompt: &str, turns: &[Turn], upto: i64, history: usize) -> Vec<Value> {
    let kept: Vec<&Turn> = turns
        .iter()
        .filter(|t| t.id <= upto && !t.heard().trim().is_empty())
        .collect();
    let skip = kept.len().saturating_sub(history + 1);
    let mut out = vec![json!({ "role": "system", "content": prompt })];
    for t in kept.into_iter().skip(skip) {
        let heard = t.heard().trim();
        let (role, content) = match t.role {
            Role::User => ("user", heard.to_string()),
            Role::Assistant if t.cut() => ("assistant", format!("{heard}{CUT_NOTE}")),
            Role::Assistant => ("assistant", heard.to_string()),
        };
        out.push(json!({ "role": role, "content": content }));
    }
    out
}

/// Stream the model's answer to `say` into turn `reply`.
fn answer(db: &Db, llm: &Llm, messages: &[Value], say: &Turn, reply: i64) -> Result<()> {
    let url = llm.chat_url();
    let req = llm.post(&url);
    let chars: usize = messages
        .iter()
        .map(|m| m["content"].as_str().map_or(0, str::len))
        .sum();
    info!(
        id = say.id,
        reply,
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
    info!(id = say.id, ms = start.elapsed().as_millis() as u64, "stream opened");
    let mut stats = Stats::default();
    let result = stream(db, say.rev, reply, resp, start, &mut stats);
    info!(
        id = say.id,
        reply,
        total_ms = start.elapsed().as_millis() as u64,
        first_thought_ms = stats.first_thought,
        first_word_ms = stats.first_word,
        reasoning_chars = stats.reasoning,
        answer_chars = stats.answer,
        writes = stats.writes,
        finish = %stats.finish.as_deref().unwrap_or("-"),
        interrupted = stats.interrupted,
        "reply done"
    );
    if let Some(t) = &stats.timings {
        // llama.cpp's own numbers: prompt and generation speed.
        info!(
            id = say.id,
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
            id = say.id,
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
    writes: usize,
    finish: Option<String>,
    interrupted: bool,
    timings: Option<Value>,
    usage: Option<Value>,
}

/// How long deltas are gathered before the turn is written again.
/// Short enough that scribe starts speaking promptly, long enough that
/// a fast model isn't one database write per token.
const FLUSH: Duration = Duration::from_millis(120);

/// Read the server-sent events of `resp` into turn `reply`, which grows
/// as they arrive. Stops as soon as the user has written anything since
/// revision `said_at`: the answer is no longer the one that was wanted.
///
/// Deltas go in exactly as they come, spacing included; it is scribe
/// that decides where the sentences are and speaks them.
fn stream(
    db: &Db,
    said_at: i64,
    reply: i64,
    resp: ureq::Response,
    start: Instant,
    st: &mut Stats,
) -> Result<()> {
    let ms = || start.elapsed().as_millis() as u64;
    let mut pending = String::new();
    let mut flushed = Instant::now();
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
                info!(reply, ms = ms(), "model is thinking (reasoning, not spoken)");
            }
            st.reasoning += r.len();
            if thinking_logged.elapsed() >= Duration::from_secs(5) {
                thinking_logged = Instant::now();
                info!(
                    reply,
                    ms = ms(),
                    reasoning_chars = st.reasoning,
                    "still thinking"
                );
            }
            debug!(reply, piece = r, "reasoning");
        }
        if let Some(piece) = delta["content"].as_str().filter(|p| !p.is_empty()) {
            if st.first_word.is_none() {
                st.first_word = Some(ms());
                info!(reply, ms = ms(), "first words");
            }
            st.answer += piece.len();
            debug!(reply, piece, "content");
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
            if db.said_since(said_at)? {
                st.interrupted = true;
                db.extend(reply, "", true)?;
                return Ok(());
            }
        }
        if !pending.is_empty() && flushed.elapsed() >= FLUSH {
            flushed = Instant::now();
            info!(reply, ms = ms(), text = %pending.trim(), "reply");
            db.extend(reply, &pending, false)?;
            pending.clear();
            st.writes += 1;
        }
    }
    if db.said_since(said_at)? {
        st.interrupted = true;
        db.extend(reply, "", true)?;
        return Ok(());
    }
    if !pending.trim().is_empty() {
        info!(reply, ms = ms(), text = %pending.trim(), "reply");
    }
    // The last write says the reply is complete, even if it adds nothing.
    db.extend(reply, &pending, true)?;
    st.writes += 1;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(id: i64, role: Role, text: &str, spoken: Option<usize>) -> Turn {
        Turn {
            id,
            conversation: 1,
            role,
            text: text.into(),
            rev: id,
            done: true,
            spoken,
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
        let turns = [
            turn(1, Role::User, "Hi.", None),
            turn(2, Role::Assistant, "Hello. How can I help you today?", Some(2)),
            turn(3, Role::User, "Tell me a joke.", None),
        ];
        let m = conversation("P", &turns, 3, 40);
        let pairs: Vec<(&str, &str)> = m
            .iter()
            .map(|v| (v["role"].as_str().unwrap(), v["content"].as_str().unwrap()))
            .collect();
        // The reply goes in as the two words that reached the ear.
        assert_eq!(
            pairs,
            vec![
                ("system", "P"),
                ("user", "Hi."),
                ("assistant", "Hello. How [the user interrupted here]"),
                ("user", "Tell me a joke."),
            ]
        );
        // Nothing after the turn being answered, and history is capped
        // at the newest messages.
        assert_eq!(conversation("P", &turns, 1, 40).len(), 2);
        assert_eq!(conversation("P", &turns, 3, 1).len(), 3);
    }

    /// A reply heard to the end is quoted plainly, and one the user
    /// never heard a word of is left out of the history altogether.
    #[test]
    fn whole_replies_and_unheard_ones() {
        let turns = [
            turn(1, Role::User, "Hi.", None),
            turn(2, Role::Assistant, "Hello there.", Some(2)),
            turn(3, Role::User, "Again.", None),
            turn(4, Role::Assistant, "Hello there.", Some(0)),
            turn(5, Role::User, "Once more.", None),
        ];
        let contents: Vec<String> = conversation("P", &turns, 5, 40)
            .iter()
            .map(|v| v["content"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            contents,
            vec!["P", "Hi.", "Hello there.", "Again.", "Once more."]
        );
    }
}
