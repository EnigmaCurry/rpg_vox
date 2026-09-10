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

use super::{Sink, SavePromptOutcome, SynthMode, VoiceOverride, comfyui::decode_audio_chunk};

pub struct Config {
    /// Gradio base URL of the Qwen3-TTS-CustomVoice deploy (preset speakers
    /// + instruct-style control). Required — this is the "always available"
    /// mode; clone/design fall back to it if their dedicated URLs are unset.
    pub presets_url: String,
    /// Optional Qwen3-TTS-Base deploy URL (3-second reference-audio clone).
    /// When None, clone-mode configs synth via the presets URL and get a
    /// preset voice instead.
    pub clone_url: Option<String>,
    /// Optional Qwen3-TTS-VoiceDesign deploy URL. When None, design-mode
    /// configs synth via the presets URL.
    pub design_url: Option<String>,
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
    presets_url: String,
    clone_url: Option<String>,
    design_url: Option<String>,
    speaker: String,
    language: String,
    instruct: String,
}

fn validate_http_url(url: &str, name: &str) -> Result<String> {
    let trimmed = url.trim_end_matches('/').to_string();
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return Err(anyhow!(
            "qwen3 {name} URL must start with http:// or https:// (got {trimmed:?})"
        ));
    }
    Ok(trimmed)
}

impl Backend {
    pub fn new(cfg: Config) -> Result<Self> {
        let presets_url = validate_http_url(&cfg.presets_url, "presets")?;
        let clone_url = cfg
            .clone_url
            .as_deref()
            .map(|u| validate_http_url(u, "clone"))
            .transpose()?;
        let design_url = cfg
            .design_url
            .as_deref()
            .map(|u| validate_http_url(u, "design"))
            .transpose()?;
        let http = HttpClient::builder()
            .timeout(cfg.request_timeout)
            .build()
            .context("building qwen3 http client")?;
        Ok(Self {
            http,
            presets_url,
            clone_url,
            design_url,
            speaker: cfg.speaker,
            language: cfg.language,
            instruct: cfg.instruct,
        })
    }

    /// Route text through the mode-appropriate Gradio endpoint:
    ///
    /// * [`SynthMode::Preset`] → `run_instruct` on the presets URL, with
    ///   optional per-call speaker/language/instruct overrides.
    /// * [`SynthMode::Design`] → `run_voice_design` on the design URL.
    /// * [`SynthMode::Clone`]  → upload the compact voice-prompt file to
    ///   the clone URL's `/upload`, then `load_prompt_and_gen`.
    ///
    /// Clone/Design require their dedicated URL to be configured — falling
    /// back to the presets URL would silently swap voices, which is worse
    /// than a clear "backend not configured" error at synth time.
    pub async fn synthesize(
        &mut self,
        text: &str,
        voice: &VoiceOverride,
        sink: &mut Sink<'_>,
    ) -> Result<()> {
        let language = voice
            .language
            .as_deref()
            .unwrap_or(&self.language)
            .to_string();

        let (base_url, endpoint, event_id) = match &voice.mode {
            SynthMode::Preset => {
                let speaker = voice.speaker.as_deref().unwrap_or(&self.speaker);
                let instruct = voice.instruct.as_deref().unwrap_or(&self.instruct);
                info!(
                    mode = "preset",
                    %speaker,
                    %language,
                    chars = text.len(),
                    instruct_chars = instruct.len(),
                    "qwen3 remote: submitting"
                );
                let event_id = self
                    .submit_call(
                        &self.presets_url,
                        "run_instruct",
                        // Gradio expects `data` positional in declared param
                        // order: (text, lang_disp, spk_disp, instruct).
                        serde_json::json!([text, language, speaker, instruct]),
                    )
                    .await?;
                (self.presets_url.clone(), "run_instruct", event_id)
            }
            SynthMode::Design { description } => {
                let base = self.design_url.as_deref().ok_or_else(|| {
                    anyhow!(
                        "voice profile is set to Design mode but RPG_VOX_QWEN3_DESIGN_URL is not configured"
                    )
                })?;
                info!(
                    mode = "design",
                    %language,
                    chars = text.len(),
                    design_chars = description.len(),
                    "qwen3 remote: submitting"
                );
                let event_id = self
                    .submit_call(
                        base,
                        "run_voice_design",
                        // Endpoint sig: (text, lang_disp, design).
                        serde_json::json!([text, language, description]),
                    )
                    .await?;
                (base.to_string(), "run_voice_design", event_id)
            }
            SynthMode::Clone {
                voice_file_bytes,
                voice_file_name,
            } => {
                let base = self.clone_url.as_deref().ok_or_else(|| {
                    anyhow!(
                        "voice profile is set to Clone mode but RPG_VOX_QWEN3_CLONE_URL is not configured"
                    )
                })?;
                // Upload the compact voice-prompt file so Gradio has a
                // path we can reference in the /call body. Gradio /tmp
                // eviction is on its background sweeper (see earlier
                // analysis) — no cleanup call needed on our side.
                let uploaded_path = self
                    .upload_file(base, voice_file_name, voice_file_bytes.clone())
                    .await?;
                info!(
                    mode = "clone",
                    %language,
                    %uploaded_path,
                    voice_file_bytes = voice_file_bytes.len(),
                    chars = text.len(),
                    "qwen3 remote: submitting"
                );
                let event_id = self
                    .submit_call(
                        base,
                        "load_prompt_and_gen",
                        // Endpoint sig: (file_obj, text, lang_disp).
                        // File params in modern Gradio APIs go as an object
                        // with `path` + gradio.FileData meta so the server
                        // reconstructs the FileData wrapper.
                        serde_json::json!([
                            {
                                "path": uploaded_path,
                                "orig_name": voice_file_name,
                                "meta": {"_type": "gradio.FileData"},
                            },
                            text,
                            language,
                        ]),
                    )
                    .await?;
                (base.to_string(), "load_prompt_and_gen", event_id)
            }
        };

        let audio_url = self.await_result(&base_url, endpoint, &event_id).await?;
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

    /// Distill a reference wav into a compact voice-prompt file via the
    /// clone deploy's `/save_prompt`. Returns the raw bytes of the voice
    /// file plus the filename Gradio gave it (extension baked in).
    ///
    /// Only reachable when the clone URL was configured at startup — the
    /// runner dispatcher checks the active backend variant before this
    /// method is called, so the None branch below is a safety net for
    /// runtime config changes we don't yet support.
    pub async fn save_prompt(
        &self,
        reference_wav: Vec<u8>,
        reference_wav_name: String,
        ref_txt: String,
        use_xvec: bool,
    ) -> Result<SavePromptOutcome> {
        let base = self.clone_url.as_deref().ok_or_else(|| {
            anyhow!("RPG_VOX_QWEN3_CLONE_URL is not configured; cannot save voice prompt")
        })?;
        info!(
            %base,
            wav_bytes = reference_wav.len(),
            ref_txt_chars = ref_txt.len(),
            %use_xvec,
            "qwen3 remote: save_prompt starting"
        );
        let uploaded_wav = self
            .upload_file(base, &reference_wav_name, reference_wav)
            .await?;
        let event_id = self
            .submit_call(
                base,
                "save_prompt",
                serde_json::json!([
                    {
                        "path": uploaded_wav,
                        "orig_name": reference_wav_name,
                        "meta": {"_type": "gradio.FileData"},
                    },
                    ref_txt,
                    use_xvec,
                ]),
            )
            .await?;
        let voice_file_url = self.await_result(base, "save_prompt", &event_id).await?;
        let voice_file_bytes = self
            .http
            .get(&voice_file_url)
            .send()
            .await
            .with_context(|| format!("GET {voice_file_url}"))?
            .error_for_status()
            .with_context(|| format!("GET {voice_file_url}"))?
            .bytes()
            .await
            .context("reading qwen3 voice-prompt response body")?
            .to_vec();
        // Filename is the last path segment of the URL (strip query if any).
        // Gradio URLs are `https://…/gradio_api/file=/tmp/gradio/hash/foo.bin`;
        // we want `foo.bin` so the extension propagates through storage +
        // future uploads.
        let filename = voice_file_url
            .rsplit('/')
            .next()
            .and_then(|s| s.split('?').next())
            .unwrap_or("voice.bin")
            .to_string();
        info!(
            bytes = voice_file_bytes.len(),
            %filename,
            "qwen3 remote: save_prompt complete"
        );
        Ok(SavePromptOutcome {
            voice_file_bytes,
            filename,
        })
    }

    /// Push a file into a Gradio deploy's `/gradio_api/upload` endpoint and
    /// return the server-side path we can then reference in a `/call/`
    /// body. `filename` is used as the multipart part filename; Gradio
    /// preserves the extension in the returned temp path.
    async fn upload_file(
        &self,
        base_url: &str,
        filename: &str,
        bytes: Vec<u8>,
    ) -> Result<String> {
        let url = format!("{base_url}/gradio_api/upload");
        let form = reqwest::multipart::Form::new().part(
            "files",
            reqwest::multipart::Part::bytes(bytes)
                .file_name(filename.to_string())
                .mime_str("application/octet-stream")
                .context("setting upload part mime")?,
        );
        let resp = self
            .http
            .post(&url)
            .multipart(form)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?
            .error_for_status()
            .with_context(|| format!("POST {url}"))?;
        let paths: Vec<String> = resp
            .json()
            .await
            .context("parsing gradio /upload response (expected string array)")?;
        paths
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("gradio /upload returned empty array"))
    }

    /// Generic POST to `/gradio_api/call/{endpoint}` — returns the event id
    /// that the SSE loop in [`Self::await_result`] then polls.
    async fn submit_call(
        &self,
        base_url: &str,
        endpoint: &str,
        data: serde_json::Value,
    ) -> Result<String> {
        let url = format!("{base_url}/gradio_api/call/{endpoint}");
        let body = serde_json::json!({ "data": data });
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
        debug!(%event_id, %endpoint, "qwen3 remote: got event_id");
        Ok(event_id)
    }

    async fn await_result(&self, base_url: &str, endpoint: &str, event_id: &str) -> Result<String> {
        let url = format!("{base_url}/gradio_api/call/{endpoint}/{event_id}");
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
                            return parse_complete_payload(payload, base_url);
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
