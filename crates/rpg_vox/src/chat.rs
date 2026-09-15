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
use tracing::{debug, warn};

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

/// Stateless LLM wrapper. `history` used to live in the client itself; the
/// switch to multi-script moved it into the sqlite store so each /scripts
/// row has its own conversation. Callers now build a `Vec<ChatMessage>`
/// from the script's stored turns and hand it to `generate_reply`.
#[derive(Clone)]
pub struct Client {
    cfg: Config,
    http: reqwest::Client,
}

impl Client {
    pub fn new(cfg: Config) -> Self {
        Self {
            cfg,
            http: reqwest::Client::new(),
        }
    }

    pub fn model(&self) -> &str {
        &self.cfg.model
    }

    pub fn base_url(&self) -> &str {
        &self.cfg.base_url
    }

    /// Send a rolling history to the LLM and return the cleaned reply.
    /// Failures leave the caller's history untouched (nothing is written
    /// back here) — the caller decides whether to retry or roll back.
    pub async fn generate_reply(
        &self,
        history: Vec<ChatMessage>,
        system_prompt_override: Option<String>,
    ) -> Result<String> {
        let messages = self.build_messages(&history, system_prompt_override.as_deref());
        let raw = self.completion(&messages, None).await?;
        debug!(bytes = raw.len(), raw = %raw, "chat completion raw content");
        // OpenAI Harmony-format models (gpt-oss and its finetunes like
        // Muse-Glimmer) reply on channels: `to=self<|message|>…<|eom|>`
        // for their internal scratchpad and `to=user<|message|>…<|eot|>`
        // for the visible reply. llama.cpp's `--reasoning-format none`
        // leaves both in `content`. Extract just the user channel here so
        // the strip_thinking pass below only sees the reply, and the
        // scratchpad never reaches the UI or TTS. Non-Harmony replies
        // pass through untouched.
        let base = extract_harmony_user_reply(&raw).unwrap_or_else(|| raw.clone());
        let cleaned = strip_thinking(&base).trim().to_string();
        if cleaned.is_empty() {
            warn!(bytes = raw.len(), raw = %raw, "LLM returned empty content after cleanup");
            return Err(anyhow!(
                "LLM returned empty content after cleanup (raw {} bytes: {:?})",
                raw.len(),
                if raw.len() > 200 { format!("{}…", &raw[..200]) } else { raw }
            ));
        }
        Ok(cleaned)
    }

    /// Same as [`Self::generate_reply`] but constrains the model output to
    /// a JSON schema via the OpenAI-standard `response_format` field.
    /// llama.cpp's OpenAI-compatible server honors this on recent builds
    /// (grammar-constrained decoding). `disable_thinking` is passed
    /// through as configured — the caller may want reasoning + JSON.
    ///
    /// Returns the parsed JSON value on success; on JSON parse failure
    /// includes the raw content (truncated to 400 chars) in the error.
    pub async fn generate_reply_json(
        &self,
        history: Vec<ChatMessage>,
        system_prompt_override: Option<String>,
        json_schema: Value,
    ) -> Result<Value> {
        let messages = self.build_messages(&history, system_prompt_override.as_deref());
        let response_format = serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": "paragraphs",
                "strict": true,
                "schema": json_schema,
            }
        });
        let raw = self.completion(&messages, Some(response_format)).await?;
        debug!(bytes = raw.len(), raw = %raw, "chat json completion raw content");
        let base = extract_harmony_user_reply(&raw).unwrap_or_else(|| raw.clone());
        let cleaned = strip_thinking(&base).trim().to_string();
        if cleaned.is_empty() {
            warn!(bytes = raw.len(), raw = %raw, "LLM returned empty JSON content after cleanup");
            return Err(anyhow!(
                "LLM returned empty JSON content after cleanup (raw {} bytes: {:?})",
                raw.len(),
                if raw.len() > 200 { format!("{}…", &raw[..200]) } else { raw }
            ));
        }
        // First try the cleaned string as-is. If that fails, walk it
        // for the first balanced JSON array/object — recovers when a
        // Harmony-format model emits the schema-constrained payload
        // inside `to=self<|message|>` (which our extract_harmony_user_reply
        // skips because it looks for `to=user`) or wraps the JSON with
        // any other noise the strip pipeline didn't recognize.
        match serde_json::from_str::<Value>(&cleaned) {
            Ok(v) => Ok(v),
            Err(direct_err) => {
                if let Some(span) = extract_first_json(&cleaned) {
                    if let Ok(v) = serde_json::from_str::<Value>(span) {
                        return Ok(v);
                    }
                }
                let preview: String = if cleaned.len() > 400 {
                    format!("{}…", &cleaned[..400])
                } else {
                    cleaned.clone()
                };
                Err(anyhow!(
                    "LLM JSON parse failed ({direct_err}): raw={preview}"
                ))
            }
        }
    }

    fn build_messages(
        &self,
        history: &[ChatMessage],
        system_prompt_override: Option<&str>,
    ) -> Vec<ChatMessage> {
        let mut out = Vec::with_capacity(history.len() + 1);
        let sys = system_prompt_override
            .map(|s| s.to_string())
            .or_else(|| self.cfg.system_prompt.clone());
        if let Some(s) = sys.filter(|v| !v.trim().is_empty()) {
            out.push(ChatMessage {
                role: "system".into(),
                content: s,
            });
        }
        let take = self.cfg.max_history.min(history.len());
        out.extend(history[history.len() - take..].iter().cloned());
        out
    }

    async fn completion(
        &self,
        messages: &[ChatMessage],
        response_format: Option<Value>,
    ) -> Result<String> {
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
        if let Some(rf) = response_format {
            body["response_format"] = rf;
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
        let content = v
            .pointer("/choices/0/message/content")
            .and_then(|c| c.as_str())
            .ok_or_else(|| anyhow!("no choices[0].message.content in response: {v}"))?;
        // Reasoning-model servers (llama.cpp with `--reasoning-format deepseek`,
        // vLLM with `--enable-reasoning`, etc.) siphon everything the model
        // emitted between `<think>` tags into a separate `reasoning_content`
        // field, leaving `content` empty when the model produced ONLY
        // reasoning. We deliberately don't fall back to reasoning_content —
        // that's the scratchpad, not the answer, and rendering it in the
        // Script UI reads as a wall of stream-of-consciousness. Instead,
        // surface a specific error so the user knows the fix is server-side.
        if content.is_empty() {
            let has_reasoning = v
                .pointer("/choices/0/message/reasoning_content")
                .and_then(|c| c.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false);
            let finish = v
                .pointer("/choices/0/finish_reason")
                .and_then(|c| c.as_str())
                .unwrap_or("");
            warn!(
                envelope = %summarise(&text),
                finish_reason = finish,
                has_reasoning,
                "LLM content was empty"
            );
            if has_reasoning {
                return Err(anyhow!(
                    "LLM emitted only reasoning (reasoning_content present, content empty). \
                     Restart llama.cpp with `--reasoning-format none` so `<think>` blocks \
                     stay in `content` where our own strip pass can drop them, \
                     or pick a non-reasoning model."
                ));
            }
            if finish == "length" {
                return Err(anyhow!(
                    "LLM hit the token cap before producing content \
                     (finish_reason=length). Raise --chat-max-tokens \
                     (currently {}) or shorten the system prompt.",
                    self.cfg.max_tokens
                ));
            }
        }
        Ok(content.to_string())
    }
}

/// Extract the visible user-facing reply from a Harmony-format response.
///
/// Some models (gpt-oss family and finetunes such as Muse-Glimmer) reply
/// in OpenAI's Harmony chat format with multiple named channels:
///
/// * `to=self<|message|>…<|eom|>` — internal scratchpad, must be hidden.
/// * `<|start|>assistant to=user<|message|>…<|eot|>` — the actual reply.
///
/// When llama.cpp runs with `--reasoning-format none` (which we recommend
/// so the strip-thinking pass sees any `<think>` blocks), the raw channel
/// markers arrive verbatim in `content`. Find the LAST `to=user<|message|>`
/// (in case the model narrates multiple internal messages before settling
/// on the reply) and take everything up to the next end-of-message marker.
///
/// Returns `None` when no Harmony markers are found so the caller can pass
/// the raw content through to the generic strip pipeline.
pub fn extract_harmony_user_reply(s: &str) -> Option<String> {
    const MARKER: &str = "to=user<|message|>";
    let idx = s.rfind(MARKER)?;
    let start = idx + MARKER.len();
    let rest = &s[start..];
    // Any of these mark the end of a Harmony message. Truncate at whichever
    // appears first — a trailing `<|eot|>` is the norm but a truncated reply
    // (finish_reason=length) can end with none of them.
    let end = ["<|eot|>", "<|end|>", "<|eom|>", "<|return|>"]
        .iter()
        .filter_map(|tag| rest.find(tag))
        .min()
        .unwrap_or(rest.len());
    Some(rest[..end].trim().to_string())
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

/// Find the first balanced JSON array or object in `s` and return its
/// substring. Tracks bracket depth while respecting quoted strings and
/// backslash escapes, so structural characters inside string literals
/// don't throw off the count. Returns `None` when no complete
/// expression is found (either no opener present, or the input is
/// truncated mid-value).
///
/// Used as a fallback in `generate_reply_json` when the Harmony /
/// think-strip pipeline leaves stray wrapper text next to the JSON —
/// e.g. `to=self<|message|>[…]<|eom|>` when the model emits the
/// structured payload in the reasoning channel instead of the user
/// channel.
fn extract_first_json(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let start = bytes
        .iter()
        .position(|b| *b == b'[' || *b == b'{')?;
    let open = bytes[start];
    let close = if open == b'[' { b']' } else { b'}' };
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut escape = false;
    for i in start..bytes.len() {
        let b = bytes[i];
        if in_str {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            x if x == open => depth += 1,
            x if x == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
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
    use super::{extract_first_json, extract_harmony_user_reply, strip_thinking};

    #[test]
    fn extracts_json_from_wrong_harmony_channel() {
        // Model emitted the schema-constrained payload inside `to=self`
        // instead of `to=user`, so extract_harmony_user_reply returned
        // None and the raw string still has Harmony wrappers. The
        // scan-for-first-JSON fallback in generate_reply_json needs to
        // recover the array anyway.
        let raw = "to=self<|message|>[{\"end_clip\":\"c1\",\"text\":\"hi\"}]<|eom|>";
        let span = extract_first_json(raw).expect("should find the array");
        let parsed: serde_json::Value = serde_json::from_str(span).expect("valid json");
        assert_eq!(parsed[0]["end_clip"].as_str(), Some("c1"));
    }

    #[test]
    fn extract_first_json_handles_strings_with_brackets() {
        // A `]` inside a string literal must not close the outer array.
        let raw = "noise before [{\"text\":\"has ] inside\"},{\"text\":\"ok\"}] trailing";
        let span = extract_first_json(raw).expect("should find the array");
        let parsed: serde_json::Value = serde_json::from_str(span).expect("valid json");
        assert_eq!(parsed.as_array().unwrap().len(), 2);
    }

    #[test]
    fn extract_first_json_none_when_truncated() {
        // No matching close bracket: never returns a partial span.
        assert!(extract_first_json("prefix [{\"a\":1},").is_none());
    }


    #[test]
    fn harmony_extracts_last_user_message() {
        let raw = "to=self<|message|>thinking...<|eom|><|start|>assistant to=user<|message|>hello there<|eot|>";
        assert_eq!(
            extract_harmony_user_reply(raw).as_deref(),
            Some("hello there"),
        );
    }

    #[test]
    fn harmony_handles_truncated_end() {
        // finish_reason=length can chop the trailing <|eot|> — still return
        // whatever text was captured after the marker.
        let raw = "to=self<|message|>x<|eom|>to=user<|message|>partial reply";
        assert_eq!(
            extract_harmony_user_reply(raw).as_deref(),
            Some("partial reply"),
        );
    }

    #[test]
    fn harmony_returns_none_for_plain_text() {
        assert!(extract_harmony_user_reply("just a normal reply").is_none());
    }

    #[test]
    fn harmony_prefers_last_user_channel() {
        // Two `to=user` blocks: take the last one (the model settled on it).
        let raw = "to=user<|message|>first<|eom|>to=user<|message|>final<|eot|>";
        assert_eq!(
            extract_harmony_user_reply(raw).as_deref(),
            Some("final"),
        );
    }

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

    #[test]
    fn json_envelope_survives_strip_pipeline() {
        // Simulate a llama.cpp reply where the model emitted `<think>`
        // reasoning followed by a Harmony `to=user` channel wrapping a
        // JSON array. The `generate_reply_json` cleanup pipeline —
        // Harmony extract → strip_thinking → trim — should leave clean
        // JSON parseable by serde_json.
        let raw = "to=self<|message|>plan the split<|eom|>\
                   <|start|>assistant to=user<|message|>\
                   <think>double-check ids</think>\
                   [{\"end_clip\":\"c1\",\"text\":\"hello world\"}]\
                   <|eot|>";
        let base = extract_harmony_user_reply(raw).unwrap();
        let cleaned = strip_thinking(&base).trim().to_string();
        let parsed: serde_json::Value = serde_json::from_str(&cleaned)
            .expect("cleaned JSON should parse");
        assert_eq!(parsed[0]["end_clip"].as_str(), Some("c1"));
        assert_eq!(parsed[0]["text"].as_str(), Some("hello world"));
    }
}
