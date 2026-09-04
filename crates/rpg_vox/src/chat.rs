//! OpenAI-compatible chat client (vLLM, ollama, llama.cpp server, …).
//!
//! Holds a rolling conversation history in-process; each `send` appends the
//! user turn, POSTs `{base_url}/chat/completions`, then appends the assistant
//! turn. `<think>…</think>` blocks emitted by Qwen3-style reasoning models are
//! stripped before the reply is returned so they don't reach the UI or the TTS
//! path.
//!
//! No streaming for now — the whole reply is awaited before returning. Chat
//! feels snappy enough at Qwen3-8B rates and it keeps the wiring simple.

use anyhow::{Context as _, Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::debug;

#[derive(Clone, Debug)]
pub struct Config {
    /// OpenAI-style base URL, ending in `/v1` (e.g. `http://127.0.0.1:8000/v1`).
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub system_prompt: Option<String>,
    /// Cap on non-system messages kept in history; older turns are dropped.
    pub max_history: usize,
    pub max_tokens: u32,
    /// When true, request `chat_template_kwargs: {enable_thinking: false}` so
    /// Qwen3-style reasoning models skip the `<think>` phase entirely. TTS
    /// wants the answer, not the reasoning, and reasoning easily blows past
    /// `max_tokens` and truncates without ever emitting a reply.
    pub disable_thinking: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Clone)]
pub struct Client {
    cfg: Config,
    http: reqwest::Client,
    history: Arc<RwLock<Vec<ChatMessage>>>,
}

impl Client {
    pub fn new(cfg: Config) -> Self {
        Self {
            cfg,
            http: reqwest::Client::new(),
            history: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn model(&self) -> &str {
        &self.cfg.model
    }

    pub fn base_url(&self) -> &str {
        &self.cfg.base_url
    }

    pub async fn history_snapshot(&self) -> Vec<ChatMessage> {
        self.history.read().await.clone()
    }

    pub async fn reset(&self) {
        self.history.write().await.clear();
    }

    /// Append `user_text`, call the LLM, append the assistant reply, return
    /// the cleaned reply text.
    pub async fn send(&self, user_text: String) -> Result<String> {
        let messages = {
            let mut hist = self.history.write().await;
            hist.push(ChatMessage {
                role: "user".into(),
                content: user_text,
            });
            self.build_messages(&hist)
        };
        let raw = self.completion(&messages).await?;
        debug!(bytes = raw.len(), raw = %raw, "chat completion raw content");
        let cleaned = strip_thinking(&raw).trim().to_string();
        if cleaned.is_empty() {
            // Rare but possible: model emitted only <think>…</think> or nothing.
            // Drop the user turn so a retry works cleanly.
            self.history.write().await.pop();
            return Err(anyhow!("LLM returned empty content"));
        }
        self.history.write().await.push(ChatMessage {
            role: "assistant".into(),
            content: cleaned.clone(),
        });
        Ok(cleaned)
    }

    fn build_messages(&self, history: &[ChatMessage]) -> Vec<ChatMessage> {
        let mut out = Vec::with_capacity(history.len() + 1);
        if let Some(sys) = &self.cfg.system_prompt {
            out.push(ChatMessage {
                role: "system".into(),
                content: sys.clone(),
            });
        }
        let take = self.cfg.max_history.min(history.len());
        out.extend(history[history.len() - take..].iter().cloned());
        out
    }

    async fn completion(&self, messages: &[ChatMessage]) -> Result<String> {
        let url = format!(
            "{}/chat/completions",
            self.cfg.base_url.trim_end_matches('/')
        );
        let mut body = serde_json::json!({
            "model": self.cfg.model,
            "messages": messages,
            "max_tokens": self.cfg.max_tokens,
            "stream": false,
        });
        if self.cfg.disable_thinking {
            // vLLM Qwen3 reads this off the request and passes it into the
            // chat template so the `<think>\n` prefix is not injected.
            body["chat_template_kwargs"] =
                serde_json::json!({ "enable_thinking": false });
        }
        let mut req = self.http.post(&url).json(&body);
        if let Some(key) = &self.cfg.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await.with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("LLM {}: {}", status, summarise(&text)));
        }
        let v: Value = serde_json::from_str(&text)
            .with_context(|| format!("parsing LLM response: {}", summarise(&text)))?;
        // Only `content` is spoken. `reasoning_content` (when the server has
        // a reasoning parser configured) is intentionally ignored — TTS is
        // for the answer, not the scratchpad.
        let content = v
            .pointer("/choices/0/message/content")
            .and_then(|c| c.as_str())
            .ok_or_else(|| anyhow!("no choices[0].message.content in response: {v}"))?;
        Ok(content.to_string())
    }
}

/// Remove reasoning blocks from an LLM reply.
///
/// Handles three formats seen in the wild:
///
/// * Balanced pairs: `<think>…</think>reply` → `reply`.
/// * Implicit-open (Qwen3 chat template pre-emits `<think>\n` as part of the
///   prompt, so the model's response text has no opening tag and starts with
///   the reasoning body itself): `reasoning</think>reply` → `reply`.
/// * Unclosed opening (model hit max_tokens mid-thought): `visible<think>…`
///   → `visible`.
///
/// Recognizes `<think>`, `<thinking>`, `<reasoning>` case-insensitively.
pub fn strip_thinking(s: &str) -> String {
    const TAGS: &[&str] = &["think", "thinking", "reasoning"];

    // 1. Strip all balanced <tag>…</tag> pairs. Loop until no more matches so
    // interleaved blocks all get cleared.
    let mut result = s.to_string();
    loop {
        let mut stripped = false;
        let lower = result.to_ascii_lowercase();
        for tag in TAGS {
            let open = format!("<{tag}>");
            let close = format!("</{tag}>");
            let Some(op) = lower.find(&open) else { continue };
            let Some(cp) = lower[op + open.len()..].find(&close) else { continue };
            let close_end = op + open.len() + cp + close.len();
            let mut rebuilt = String::with_capacity(result.len());
            rebuilt.push_str(&result[..op]);
            rebuilt.push_str(&result[close_end..]);
            result = rebuilt;
            stripped = true;
            break;
        }
        if !stripped {
            break;
        }
    }

    // 2. Implicit-open: an orphan closing tag means everything before it (and
    // the tag itself) is reasoning.
    let lower = result.to_ascii_lowercase();
    let earliest_close_end = TAGS
        .iter()
        .filter_map(|tag| {
            let close = format!("</{tag}>");
            lower.find(&close).map(|p| p + close.len())
        })
        .min();
    if let Some(end) = earliest_close_end {
        result = result[end..].to_string();
    }

    // 3. Orphan opening tag (truncated mid-thought): drop everything from it.
    let lower = result.to_ascii_lowercase();
    let earliest_open = TAGS
        .iter()
        .filter_map(|tag| {
            let open = format!("<{tag}>");
            lower.find(&open)
        })
        .min();
    if let Some(start) = earliest_open {
        result.truncate(start);
    }

    result
}

fn summarise(body: &str) -> String {
    let flat: String = body
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    if flat.len() > 400 {
        format!("{}…", &flat[..400])
    } else {
        flat
    }
}

#[cfg(test)]
mod tests {
    use super::strip_thinking;

    #[test]
    fn strips_single_block() {
        assert_eq!(strip_thinking("<think>hmm</think>hello"), "hello");
    }

    #[test]
    fn strips_multiple_blocks() {
        assert_eq!(
            strip_thinking("<think>a</think>x<think>b</think>y"),
            "xy"
        );
    }

    #[test]
    fn drops_unclosed_tail() {
        assert_eq!(strip_thinking("visible<think>never ended"), "visible");
    }

    #[test]
    fn passthrough_without_tags() {
        assert_eq!(strip_thinking("just words"), "just words");
    }

    #[test]
    fn strips_thinking_alias() {
        assert_eq!(
            strip_thinking("<thinking>hmm</thinking>hi"),
            "hi"
        );
    }

    #[test]
    fn strips_reasoning_alias() {
        assert_eq!(
            strip_thinking("<reasoning>plan</reasoning>go"),
            "go"
        );
    }

    #[test]
    fn strips_case_insensitively() {
        assert_eq!(
            strip_thinking("<Think>hmm</THINK>hi"),
            "hi"
        );
    }

    #[test]
    fn strips_implicit_open_qwen3() {
        // Qwen3's chat template pre-emits `<think>\n`, so the model text has
        // no opening tag — reasoning body up to `</think>` is still garbage.
        assert_eq!(
            strip_thinking("reasoning goes here\n</think>\n\nreal reply"),
            "\n\nreal reply"
        );
    }

    #[test]
    fn strips_implicit_open_then_trims_downstream() {
        // Same as above but demonstrates the runner-side trim() takes care of
        // the leading whitespace after stripping.
        let out = strip_thinking("thoughts</think>final answer");
        assert_eq!(out, "final answer");
    }
}
