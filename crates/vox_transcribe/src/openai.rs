//! Pass-4 corrector backed by any OpenAI-compatible chat endpoint
//! (OpenAI, llama.cpp server, ollama, vLLM, …).

use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, Context as _, Result};
use serde::Deserialize;
use serde_json::json;
use tracing::warn;

use crate::correct::{Corrector, Edit};

/// Where the LLM is unless VOX_SCRIBE_LLM_URL says otherwise: a local
/// OpenAI-compatible server (llama.cpp, ollama, vLLM, …).
pub const DEFAULT_URL: &str = "http://127.0.0.1:9931/v1";

/// The LLM service, shared by scribe's pass 4 and `agent`, from the
/// environment:
///
/// * `VOX_SCRIBE_LLM_URL`: base URL ending in `/v1`, default [`DEFAULT_URL`]
/// * `VOX_SCRIBE_LLM_MODEL`: model name
/// * `VOX_SCRIBE_LLM_KEY`: API key; `OPENAI_API_KEY` is used too, but
///   only when the URL is OpenAI's, so it never goes to another server
#[derive(Clone, Debug)]
pub struct Endpoint {
    pub url: String,
    pub model: Option<String>,
    pub api_key: Option<String>,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

impl Endpoint {
    pub fn from_env() -> Self {
        Self::at(env("VOX_SCRIBE_LLM_URL").unwrap_or_else(|| DEFAULT_URL.into()))
    }

    /// The service at `url` (e.g. from a command-line flag), with the
    /// model and key from the environment.
    pub fn at(url: String) -> Self {
        let api_key = env("VOX_SCRIBE_LLM_KEY").or_else(|| {
            url.contains("api.openai.com")
                .then(|| env("OPENAI_API_KEY"))
                .flatten()
        });
        Self {
            model: env("VOX_SCRIBE_LLM_MODEL"),
            url,
            api_key,
        }
    }
}

#[derive(Clone, Debug)]
pub struct OpenAiConfig {
    /// Base URL ending in `/v1`, e.g. `https://api.openai.com/v1`.
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    /// Names and terms the speaker uses, for spelling fixes.
    pub vocabulary: Vec<String>,
    pub timeout: Duration,
}

pub struct OpenAiCorrector {
    cfg: OpenAiConfig,
    agent: ureq::Agent,
    system_prompt: String,
    /// Optional request parameters the server rejected; left out from then
    /// on. Some models only accept the default temperature, and some
    /// servers don't support `response_format`.
    dropped: Mutex<Vec<&'static str>>,
}

/// Optional parameters, tried in this order and dropped one by one when
/// a 400 names them.
const OPTIONAL_PARAMS: [&str; 3] = ["temperature", "response_format", "chat_template_kwargs"];

const PROMPT: &str = "\
You proofread live speech-to-text transcripts. The text was produced by an \
acoustic model, so it contains recognition errors. Fix only those:
- numbers, times and dates in the wrong format (\"11,42 PM\" -> \"11:42 PM\", \
\"One,42\" -> \"1:42\")
- misheard or misspelled words and names, using the vocabulary when given
- sentence breaks or punctuation inserted in the middle of a sentence, and \
missing sentence-ending punctuation
- capitalization
Do not rephrase, summarize, reorder, or remove words the speaker said \
(including filler words). Do not change dialect or grammar that is plausibly \
what was spoken. When unsure, leave it.

Reply with JSON only: {\"edits\": [{\"from\": \"exact text\", \"to\": \"replacement\"}]}. \
Each `from` must be copied exactly from the paragraph and be short (a few \
words around the error). Return {\"edits\": []} if nothing needs fixing.";

impl OpenAiCorrector {
    pub fn new(cfg: OpenAiConfig) -> Self {
        let mut system_prompt = PROMPT.to_string();
        if !cfg.vocabulary.is_empty() {
            system_prompt.push_str("\n\nVocabulary: ");
            system_prompt.push_str(&cfg.vocabulary.join(", "));
        }
        let agent = ureq::AgentBuilder::new().timeout(cfg.timeout).build();
        Self {
            dropped: Mutex::new(Vec::new()),
            cfg,
            agent,
            system_prompt,
        }
    }
}

impl OpenAiCorrector {
    /// One round trip with a throwaway sentence, so a bad URL, key or
    /// model fails at startup instead of on every paragraph.
    pub fn check(&self) -> Result<()> {
        self.correct("This is a test.", &[]).map(|_| ())
    }
}

#[derive(Deserialize)]
struct EditList {
    edits: Vec<Edit>,
}

impl Corrector for OpenAiCorrector {
    fn correct(&self, text: &str, context: &[String]) -> Result<Vec<Edit>> {
        let mut user = String::new();
        if !context.is_empty() {
            user.push_str("Earlier paragraphs (context only, do not edit):\n");
            for c in context {
                user.push_str(c);
                user.push_str("\n\n");
            }
        }
        user.push_str("Paragraph to proofread:\n");
        user.push_str(text);

        let url = format!(
            "{}/chat/completions",
            self.cfg.base_url.trim_end_matches('/')
        );
        let resp = loop {
            let dropped = self.dropped.lock().expect("dropped lock").clone();
            let mut body = json!({
                "model": self.cfg.model,
                "messages": [
                    { "role": "system", "content": self.system_prompt },
                    { "role": "user", "content": user },
                ],
            });
            if !dropped.contains(&"temperature") {
                body["temperature"] = json!(0);
            }
            if !dropped.contains(&"response_format") {
                body["response_format"] = json!({ "type": "json_object" });
            }
            // A reasoning model's thinking would only slow proofreading
            // down (llama.cpp's switch for it).
            if !dropped.contains(&"chat_template_kwargs") {
                body["chat_template_kwargs"] = json!({ "enable_thinking": false });
            }
            let mut req = self.agent.post(&url);
            if let Some(key) = &self.cfg.api_key {
                req = req.set("Authorization", &format!("Bearer {key}"));
            }
            match req.send_json(body) {
                Ok(r) => {
                    break r
                        .into_json::<serde_json::Value>()
                        .context("decode chat response")?
                }
                Err(ureq::Error::Status(code, r)) => {
                    let body = r.into_string().unwrap_or_default();
                    // OpenAI-style servers put a readable reason in error.message.
                    let detail = serde_json::from_str::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|v| v["error"]["message"].as_str().map(String::from))
                        .unwrap_or(body);
                    // A 400 naming an optional parameter: drop it and retry.
                    let rejected = OPTIONAL_PARAMS
                        .into_iter()
                        .find(|p| code == 400 && !dropped.contains(p) && detail.contains(p));
                    match rejected {
                        Some(param) => {
                            warn!(
                                param,
                                "LLM endpoint rejected parameter; retrying without it"
                            );
                            self.dropped.lock().expect("dropped lock").push(param);
                        }
                        None => return Err(anyhow!("{url} returned {code}: {}", detail.trim())),
                    }
                }
                Err(e) => return Err(anyhow!("{e}")),
            }
        };
        let content = resp["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow!("no message content in chat response"))?;
        parse_edits(content)
    }
}

/// Pull the edit list out of a reply, tolerating `<think>` blocks and
/// markdown code fences around the JSON.
fn parse_edits(content: &str) -> Result<Vec<Edit>> {
    let content = match content.rfind("</think>") {
        Some(i) => &content[i + "</think>".len()..],
        None => content,
    };
    let start = content
        .find('{')
        .ok_or_else(|| anyhow!("no JSON in reply: {content}"))?;
    let end = content
        .rfind('}')
        .ok_or_else(|| anyhow!("no JSON in reply: {content}"))?;
    let list: EditList = serde_json::from_str(&content[start..=end])
        .with_context(|| format!("bad edit JSON: {content}"))?;
    Ok(list.edits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wrapped_json() {
        let reply =
            "<think>hmm</think>\n```json\n{\"edits\": [{\"from\": \"a\", \"to\": \"b\"}]}\n```";
        assert_eq!(
            parse_edits(reply).unwrap(),
            vec![Edit {
                from: "a".into(),
                to: "b".into()
            }]
        );
        assert!(parse_edits("{\"edits\": []}").unwrap().is_empty());
        assert!(parse_edits("sorry").is_err());
    }
}
