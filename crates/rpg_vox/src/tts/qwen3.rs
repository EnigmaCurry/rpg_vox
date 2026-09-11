//! Remote Qwen3-TTS backend against vLLM-Omni's OpenAI-compatible speech API.
//!
//! Each container in `qwen3-tts/docker-compose.yml` serves a single model
//! variant behind its own hostname; this backend holds one URL per variant
//! and routes the request based on the [`SynthMode`] the caller picked:
//!
//! * [`SynthMode::Preset`] → presets URL, `{voice, [instructions]}`.
//! * [`SynthMode::Design`] → design URL, `task_type=VoiceDesign` +
//!   `instructions` (the free-text description).
//! * [`SynthMode::Clone`]  → clone URL, `voice=<name>` + `task_type=Base`.
//!   The voice must have been pre-registered via [`Backend::save_prompt`].
//!
//! Endpoint: `POST /v1/audio/speech` returns the whole WAV as one binary
//! response (Content-Type: audio/wav). No streaming, no chunking, no SSE
//! — a single request-per-utterance flow that the shared decoder can
//! ingest via [`super::comfyui::decode_audio_chunk`]. First-audio latency
//! = server synth wall time + one HTTP roundtrip; on this GPU that's
//! sub-second warm even for multi-sentence input (RTF ~0.1).
//!
//! Voice cloning uses `POST /v1/audio/voices` (multipart) to upload the
//! reference wav ONCE. The vLLM-Omni server persists it under
//! `SPEAKER_SAMPLES_DIR` and thereafter accepts the caller-chosen name
//! in place of per-request `ref_audio` bytes. Storage is server-side by
//! design — the rpg_vox `data/voices/` dir keeps only the original wav
//! for the "listen to what you uploaded" UI, no per-voice `.bin` blob.

use anyhow::{Context, Result, anyhow};
use reqwest::Client as HttpClient;
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;
use tracing::{debug, info};

use super::{Sink, SavePromptOutcome, SynthMode, VoiceOverride, comfyui::decode_audio_chunk};

pub struct Config {
    /// Base URL of the CustomVoice (presets) deploy — always required.
    /// Serves the preset speakers (Ryan/Serena/Vivian/…) plus `instructions`
    /// style modulation. Clone / Design fall through to a clear "backend
    /// not configured" error at synth time when their URLs are unset;
    /// silently swapping to the presets URL would produce the wrong voice.
    pub presets_url: String,
    /// Optional Base deploy URL (3-second reference-audio clone).
    pub clone_url: Option<String>,
    /// Optional VoiceDesign deploy URL (voice from free-text description).
    pub design_url: Option<String>,
    /// Default preset speaker name (e.g. `"ryan"`). Used when a per-call
    /// [`VoiceOverride`] doesn't specify one.
    pub speaker: String,
    /// Default language, e.g. `"English"`. Every mode passes this through.
    pub language: String,
    /// Default `instructions` payload for Preset mode (voice style prompt).
    /// Empty string is fine — Qwen3-TTS-CustomVoice accepts an empty
    /// instructions field.
    pub instruct: String,
    /// Per-request timeout. Sized for cold-start synth of long utterances
    /// on the remote GPU (first call after container boot can take 20s+
    /// while Stage-0 captures CUDA graphs).
    pub request_timeout: Duration,
}

/// `Clone` is cheap: `reqwest::Client` is `Arc<Inner>` under the hood,
/// so clones share the connection pool + TLS state. All other fields are
/// small `String`s. Cloneability is what lets the tts runner spawn
/// concurrent per-request tasks against a single backend without a
/// shared `Mutex` gate.
#[derive(Clone)]
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

    /// Route text through the mode-appropriate vLLM-Omni deploy. All three
    /// paths hit the same endpoint (`/v1/audio/speech`) — only the request
    /// body and the target host change.
    ///
    /// Clone / Design require their dedicated URL to be configured;
    /// falling back to the presets URL would silently render the wrong
    /// voice, so an unconfigured URL surfaces as a clear error instead.
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

        let (base_url, body) = match &voice.mode {
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
                let mut body = json!({
                    "input": text,
                    "voice": speaker,
                    "language": language,
                    "response_format": "wav",
                });
                if !instruct.is_empty() {
                    body["instructions"] = json!(instruct);
                }
                (self.presets_url.clone(), body)
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
                // VoiceDesign carries the free-text description in the
                // `instructions` field — same slot the CustomVoice model
                // uses for style modulation. `task_type` MUST be sent
                // explicitly ("Other omitted values select CustomVoice;
                // VoiceDesign must be specified explicitly" per the API
                // reference).
                let body = json!({
                    "input": text,
                    "task_type": "VoiceDesign",
                    "instructions": description,
                    "language": language,
                    "response_format": "wav",
                });
                (base.to_string(), body)
            }
            SynthMode::Clone { voice_name } => {
                let base = self.clone_url.as_deref().ok_or_else(|| {
                    anyhow!(
                        "voice profile is set to Clone mode but RPG_VOX_QWEN3_CLONE_URL is not configured"
                    )
                })?;
                info!(
                    mode = "clone",
                    %voice_name,
                    %language,
                    chars = text.len(),
                    "qwen3 remote: submitting"
                );
                // `task_type=Base` is required on the Base container to
                // reach the ICL path; the voice name resolves against
                // the server-side speaker registry (populated by
                // [`Self::save_prompt`]).
                let body = json!({
                    "input": text,
                    "voice": voice_name,
                    "task_type": "Base",
                    "language": language,
                    "response_format": "wav",
                });
                (base.to_string(), body)
            }
            SynthMode::Copy { .. } => {
                return Err(anyhow!(
                    "qwen3 backend cannot synthesize a Copy-mode config directly; \
                     Copy layers are resolved inside synthesize_profile"
                ));
            }
            SynthMode::Sample { .. } => {
                return Err(anyhow!(
                    "qwen3 backend cannot synthesize a Sample-mode config directly; \
                     Sample layers are resolved inside synthesize_profile"
                ));
            }
        };

        let url = format!("{base_url}/v1/audio/speech");
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "vllm-omni /v1/audio/speech returned {status}: {body_text}"
            ));
        }
        let wav_bytes = resp
            .bytes()
            .await
            .context("reading qwen3 audio response body")?;
        info!(bytes = wav_bytes.len(), "qwen3 remote: fetched audio");

        let chunk = decode_audio_chunk(&wav_bytes).context("decoding qwen3 audio")?;
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

    /// Register a reference audio clip with the Base container so future
    /// synth requests can address the voice by `name` instead of shipping
    /// `ref_audio` bytes every call.
    ///
    /// The vLLM-Omni server persists an extracted `.safetensors` under
    /// `SPEAKER_SAMPLES_DIR` (bounded by `SPEAKER_MAX_UPLOADED=1000`);
    /// once uploaded, `POST /v1/audio/speech {voice: <name>, task_type: "Base"}`
    /// looks it up straight from the LRU without redoing feature extraction.
    ///
    /// `voice_id` is the caller-minted name (a UUID from rpg_vox's store)
    /// — reused verbatim so the local DB row's primary key doubles as the
    /// server-side speaker key. `consent` is required by the API; we fill
    /// it with a synthetic marker since rpg_vox voices are always self-
    /// serve (the operator uploads their own reference, no external
    /// speaker's consent to track).
    pub async fn save_prompt(
        &self,
        voice_id: String,
        reference_wav: Vec<u8>,
        ref_txt: String,
    ) -> Result<SavePromptOutcome> {
        let base = self.clone_url.as_deref().ok_or_else(|| {
            anyhow!("RPG_VOX_QWEN3_CLONE_URL is not configured; cannot register voice")
        })?;
        info!(
            %base,
            %voice_id,
            wav_bytes = reference_wav.len(),
            ref_txt_chars = ref_txt.len(),
            "qwen3 remote: uploading voice sample"
        );
        let url = format!("{base}/v1/audio/voices");
        let form = reqwest::multipart::Form::new()
            .text("name", voice_id.clone())
            .text("consent", format!("rpg_vox:{voice_id}"))
            .text("ref_text", ref_txt)
            .part(
                "audio_sample",
                reqwest::multipart::Part::bytes(reference_wav)
                    .file_name("reference.wav")
                    .mime_str("audio/wav")
                    .context("setting audio_sample mime")?,
            );
        let resp = self
            .http
            .post(&url)
            .multipart(form)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "vllm-omni /v1/audio/voices returned {status}: {body_text}"
            ));
        }
        // Parse the {"success": true, "voice": {"name": ..., ...}} envelope
        // to double-check the server echoed back the name we chose. Any
        // mismatch is treated as a hard error — if the server renames
        // voices we'd otherwise silently orphan the local row.
        #[derive(Deserialize)]
        struct VoiceEnvelope {
            success: bool,
            voice: Option<VoiceEcho>,
            #[serde(default)]
            error: Option<serde_json::Value>,
        }
        #[derive(Deserialize)]
        struct VoiceEcho {
            name: String,
        }
        let parsed: VoiceEnvelope = serde_json::from_str(&body_text).with_context(|| {
            format!("parsing /v1/audio/voices envelope: {body_text}")
        })?;
        if !parsed.success {
            return Err(anyhow!(
                "vllm-omni /v1/audio/voices reported failure: {:?}",
                parsed.error
            ));
        }
        let echoed = parsed
            .voice
            .ok_or_else(|| anyhow!("/v1/audio/voices success=true but voice field missing"))?
            .name;
        if echoed != voice_id {
            return Err(anyhow!(
                "vllm-omni echoed voice name {echoed:?} but we requested {voice_id:?}"
            ));
        }
        debug!(voice_name = %echoed, "qwen3 remote: voice registered");
        Ok(SavePromptOutcome { voice_name: echoed })
    }
}
