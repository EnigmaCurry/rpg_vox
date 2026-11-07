//! The clone half of a Qwen3-TTS deploy, as much of it as rendering a
//! play needs: register a reference recording as a named voice, list
//! what is already registered, and synthesize a line in one of them.
//!
//! This is the same server rpg_vox talks to (`qwen3-tts/docker-compose.yml`,
//! the `clone` service, `Qwen/Qwen3-TTS-12Hz-1.7B-Base`) and the same two
//! endpoints its backend uses, but the shape of the work is different
//! enough to be worth its own client: rpg_vox synthesizes one utterance
//! at a time into a live microphone through a `Sink`, and this renders a
//! few hundred of them into files, out of order, with a cache in front.
//! There is nothing here to share but the URL.
//!
//! * `POST /v1/audio/voices` (multipart) registers a voice. The server
//!   extracts a `.safetensors` under its `SPEAKER_SAMPLES_DIR` and from
//!   then on accepts the name in place of the audio.
//! * `GET /v1/audio/voices` lists them, which is what lets a second
//!   render of the same cast upload nothing.
//! * `POST /v1/audio/speech` with `task_type: "Base"` returns the whole
//!   utterance as one 24 kHz mono 16-bit wav. No streaming, which for a
//!   batch render is a simplification rather than a cost.
//!
//! The model takes an `instructions` field and, on this deploy, ignores
//! it: the same text with and without a style instruction comes back
//! byte-identical often enough to settle the question. So nothing here
//! sends one, and performance directions are honoured in [`crate::dsp`]
//! instead.

use std::time::Duration;

use anyhow::{anyhow, bail, Context as _, Result};
use reqwest::Client as HttpClient;
use serde::Deserialize;
use serde_json::json;
use tracing::{debug, info};

/// What the server returns from a synth request: the wav bytes, exactly
/// as sent, so they can be cached without a decode/encode round trip.
pub struct Wav(pub Vec<u8>);

#[derive(Clone, Debug)]
pub struct Client {
    http: HttpClient,
    base: String,
}

impl Client {
    /// `base` is the deploy's root — the `/v1/...` paths are appended.
    ///
    /// The timeout is per request and generous: the first call after the
    /// container starts waits on CUDA graph capture, and a long narration
    /// paragraph is a second or two of synthesis after that.
    pub fn new(base: &str, request_timeout: Duration) -> Result<Self> {
        let base = base.trim_end_matches('/').to_string();
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            bail!("clone server URL must start with http:// or https:// (got {base:?})");
        }
        let http = HttpClient::builder()
            .timeout(request_timeout)
            .build()
            .context("building http client")?;
        Ok(Self { http, base })
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// Every voice name the server will answer to.
    pub async fn voices(&self) -> Result<Vec<String>> {
        #[derive(Deserialize)]
        struct Listing {
            #[serde(default)]
            voices: Vec<String>,
        }
        let url = format!("{}/v1/audio/voices", self.base);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("GET {url} returned {status}: {}", first_line(&body));
        }
        let listing: Listing = serde_json::from_str(&body)
            .with_context(|| format!("parsing voice listing: {}", first_line(&body)))?;
        Ok(listing.voices)
    }

    /// Register `wav` under `name`. `ref_text` may be empty — the server
    /// builds the voice from the audio either way — but giving what the
    /// sample says makes the clone closer.
    ///
    /// `consent` is required by the API. These are voices an operator
    /// supplied themselves, so it carries a marker naming this tool
    /// rather than anything to track.
    pub async fn register(&self, name: &str, wav: Vec<u8>, ref_text: &str) -> Result<()> {
        #[derive(Deserialize)]
        struct Envelope {
            success: bool,
            voice: Option<Echo>,
            #[serde(default)]
            error: Option<serde_json::Value>,
        }
        #[derive(Deserialize)]
        struct Echo {
            name: String,
        }

        info!(
            voice = %name,
            wav_bytes = wav.len(),
            ref_text_chars = ref_text.len(),
            "registering voice"
        );
        let url = format!("{}/v1/audio/voices", self.base);
        let form = reqwest::multipart::Form::new()
            .text("name", name.to_string())
            .text("consent", format!("radio_drama:{name}"))
            .text("ref_text", ref_text.to_string())
            .part(
                "audio_sample",
                reqwest::multipart::Part::bytes(wav)
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
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("POST {url} returned {status}: {}", first_line(&body));
        }
        let parsed: Envelope = serde_json::from_str(&body)
            .with_context(|| format!("parsing registration reply: {}", first_line(&body)))?;
        if !parsed.success {
            bail!("server refused voice {name:?}: {:?}", parsed.error);
        }
        // A name the server changed would leave every later synth
        // request asking for a voice that is not there, so disagreeing
        // about it is fatal rather than a warning.
        let echoed = parsed
            .voice
            .ok_or_else(|| anyhow!("registration succeeded but named no voice"))?
            .name;
        if echoed != name {
            bail!("asked the server to register {name:?} and it registered {echoed:?}");
        }
        debug!(voice = %name, "voice registered");
        Ok(())
    }

    /// Say `text` in `voice`. Returns the wav the server produced.
    pub async fn speak(&self, voice: &str, language: &str, text: &str) -> Result<Wav> {
        let url = format!("{}/v1/audio/speech", self.base);
        let body = json!({
            "input": text,
            "voice": voice,
            // Required on the Base deploy to reach the clone path at
            // all; omitting it selects the CustomVoice behaviour, which
            // this model is not.
            "task_type": "Base",
            "language": language,
            "response_format": "wav",
        });
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("POST {url} returned {status}: {}", first_line(&body));
        }
        let bytes = resp.bytes().await.context("reading audio response")?;
        if bytes.len() < 44 || &bytes[..4] != b"RIFF" {
            bail!(
                "server returned {} bytes that are not a wav: {}",
                bytes.len(),
                first_line(&String::from_utf8_lossy(&bytes[..bytes.len().min(200)]))
            );
        }
        debug!(
            voice,
            bytes = bytes.len(),
            chars = text.len(),
            "synthesized"
        );
        Ok(Wav(bytes.to_vec()))
    }
}

/// Server errors arrive as anything from a JSON envelope to an HTML
/// error page. One line of it is enough to tell which, and keeps a
/// Traefik 404 page out of the terminal.
fn first_line(body: &str) -> String {
    let line = body.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    if line.len() > 300 {
        format!("{}…", &line[..300])
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_without_a_scheme_is_refused() {
        let err = Client::new("qwen3-tts-clone.example", Duration::from_secs(1))
            .unwrap_err()
            .to_string();
        assert!(err.contains("http://"), "{err}");
    }

    #[test]
    fn a_trailing_slash_is_not_doubled() {
        let c = Client::new("https://example.test/", Duration::from_secs(1)).unwrap();
        assert_eq!(c.base(), "https://example.test");
    }

    #[test]
    fn an_error_body_is_cut_to_one_line() {
        assert_eq!(
            first_line("\n\nnope | 404 | Not Found\nmore\n"),
            "nope | 404 | Not Found"
        );
        assert!(first_line(&"x".repeat(500)).ends_with('…'));
    }
}
