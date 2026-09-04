//! Remote Qwen3-TTS backend over the Gradio HTTP API.
//!
//! Talks to a Gradio deployment of Qwen3-TTS (see the `/run_instruct`
//! endpoint) — inference happens on the remote GPU box, we just marshal
//! text in and WAV bytes out. No candle, no local model weights.
//!
//! Gradio's HTTP call flow is a two-hop:
//!
//! 1. `POST {base}/gradio_api/call/run_instruct` with body
//!    `{"data": [text, language, speaker, instruct]}`. Returns
//!    `{"event_id": "<id>"}`.
//! 2. `GET {base}/gradio_api/call/run_instruct/{event_id}` opens an SSE
//!    stream. We ignore `generating`/`heartbeat` frames and read the
//!    `complete` frame's JSON payload — a `[audio_file_object, status_str]`
//!    tuple where the audio object has a `url` (absolute) or `path`
//!    (relative under `/gradio_api/file=`).
//! 3. `GET {audio_url}` returns the WAV bytes; we decode via the shared
//!    [`super::comfyui::decode_audio_chunk`] and push one [`AudioChunk`]
//!    to the runner [`Sink`], which handles resampling to the pipewire
//!    target rate.
//!
//! No client-side chunking — the remote returns the full utterance as one
//! WAV, and per-call round-trip overhead dominates for short text. First-
//! audio latency = remote synthesis time + one HTTP fetch.

use anyhow::{Context, Result, anyhow};
use futures_util::StreamExt as _;
use reqwest::Client as HttpClient;
use serde::Deserialize;
use std::time::Duration;
use tracing::{debug, info};

use super::{Sink, comfyui::decode_audio_chunk};

pub struct Config {
    /// Gradio base URL, no trailing slash (e.g. `https://qwen3-tts.example.com`).
    pub base_url: String,
    /// One of: Serena, Vivian, Uncle Fu, Ryan, Aiden, Ono Anna, Sohee, Eric, Dylan.
    pub speaker: String,
    /// One of: Auto, Chinese, English, German, Italian, Portuguese, Spanish,
    /// Japanese, Korean, French, Russian.
    pub language: String,
    /// Optional voice-style instruction. Empty string is accepted.
    pub instruct: String,
    /// Per-request timeout. Sized for remote GPU synthesis of long utterances.
    pub request_timeout: Duration,
}

pub struct Backend {
    http: HttpClient,
    base_url: String,
    speaker: String,
    language: String,
    instruct: String,
}

impl Backend {
    pub fn new(cfg: Config) -> Result<Self> {
        let base_url = cfg.base_url.trim_end_matches('/').to_string();
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            return Err(anyhow!(
                "qwen3 base URL must start with http:// or https:// (got {base_url:?})"
            ));
        }
        let http = HttpClient::builder()
            .timeout(cfg.request_timeout)
            .build()
            .context("building qwen3 http client")?;
        Ok(Self {
            http,
            base_url,
            speaker: cfg.speaker,
            language: cfg.language,
            instruct: cfg.instruct,
        })
    }

    /// `instruct` overrides the configured voice-style for this one call.
    /// `None` falls back to the value from [`Config`] set at startup.
    pub async fn synthesize(
        &mut self,
        text: &str,
        instruct: Option<&str>,
        sink: &mut Sink<'_>,
    ) -> Result<()> {
        let event_id = self.submit(text, instruct.unwrap_or(&self.instruct)).await?;
        let audio_url = self.await_result(&event_id).await?;
        let chunk_bytes = self
            .http
            .get(&audio_url)
            .send()
            .await
            .with_context(|| format!("GET {audio_url}"))?
            .error_for_status()
            .with_context(|| format!("GET {audio_url}"))?
            .bytes()
            .await
            .context("reading qwen3 audio response body")?;
        info!(bytes = chunk_bytes.len(), "qwen3 remote: fetched audio");

        let chunk = decode_audio_chunk(&chunk_bytes).context("decoding qwen3 audio")?;
        let dur_s = chunk.samples.len() as f32 / chunk.sample_rate as f32;
        info!(
            samples = chunk.samples.len(),
            rate = chunk.sample_rate,
            duration_s = dur_s,
            "qwen3 remote: decoded"
        );
        sink.push(chunk).await?;
        Ok(())
    }

    async fn submit(&self, text: &str, instruct: &str) -> Result<String> {
        let url = format!("{}/gradio_api/call/run_instruct", self.base_url);
        // Gradio expects `data` positional in declared param order:
        // (text, lang_disp, spk_disp, instruct).
        let body = serde_json::json!({
            "data": [text, self.language, self.speaker, instruct],
        });
        info!(
            %url,
            speaker = %self.speaker,
            language = %self.language,
            chars = text.len(),
            instruct_chars = instruct.len(),
            "qwen3 remote: submitting"
        );
        #[derive(Deserialize)]
        struct CallResp {
            event_id: String,
        }
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?
            .error_for_status()
            .with_context(|| format!("POST {url}"))?;
        let event_id = resp
            .json::<CallResp>()
            .await
            .context("parsing gradio /call response (expected {event_id: ...})")?
            .event_id;
        debug!(%event_id, "qwen3 remote: got event_id");
        Ok(event_id)
    }

    async fn await_result(&self, event_id: &str) -> Result<String> {
        let url = format!("{}/gradio_api/call/run_instruct/{event_id}", self.base_url);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()
            .with_context(|| format!("GET {url}"))?;

        let mut stream = resp.bytes_stream();
        let mut buf = Vec::<u8>::new();
        let mut current_event = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("qwen3 SSE stream error")?;
            buf.extend_from_slice(&chunk);

            while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
                let raw: Vec<u8> = buf.drain(..=nl).collect();
                let line = std::str::from_utf8(&raw[..raw.len().saturating_sub(1)])
                    .unwrap_or("")
                    .trim_end_matches('\r');

                if line.is_empty() {
                    // End of an SSE event. Keep current_event around only until
                    // the next event's data arrives; Gradio always pairs event/
                    // data, so this is safe to clear here.
                    current_event.clear();
                    continue;
                }
                if let Some(name) = line.strip_prefix("event:") {
                    current_event = name.trim().to_string();
                    debug!(event = %current_event, "qwen3 SSE event");
                } else if let Some(payload) = line.strip_prefix("data:") {
                    let payload = payload.trim_start();
                    match current_event.as_str() {
                        "complete" => {
                            return parse_complete_payload(payload, &self.base_url);
                        }
                        "error" => {
                            return Err(anyhow!("qwen3 remote error event: {payload}"));
                        }
                        // Progress-only frames — Gradio emits "generating",
                        // "heartbeat", and empty-name keepalives during long
                        // synthesis. Log at debug and keep waiting.
                        _ => {
                            debug!(event = %current_event, len = payload.len(), "qwen3 SSE data");
                        }
                    }
                }
            }
        }
        Err(anyhow!(
            "qwen3 remote: SSE stream ended without a `complete` event"
        ))
    }
}

fn parse_complete_payload(payload: &str, base_url: &str) -> Result<String> {
    let val: serde_json::Value = serde_json::from_str(payload)
        .with_context(|| format!("parsing qwen3 complete-event JSON: {payload}"))?;
    let arr = val
        .as_array()
        .ok_or_else(|| anyhow!("qwen3 complete payload not a JSON array: {payload}"))?;
    let file = arr
        .first()
        .ok_or_else(|| anyhow!("qwen3 complete payload is empty: {payload}"))?;

    // Newer Gradio: absolute `url`. Older / self-hosted variants: `path`
    // relative to /gradio_api/file=.
    if let Some(url) = file.get("url").and_then(|v| v.as_str()) {
        return Ok(url.to_string());
    }
    if let Some(path) = file.get("path").and_then(|v| v.as_str()) {
        return Ok(format!("{base_url}/gradio_api/file={path}"));
    }
    Err(anyhow!(
        "qwen3 complete payload had no `url` or `path` field: {payload}"
    ))
}
