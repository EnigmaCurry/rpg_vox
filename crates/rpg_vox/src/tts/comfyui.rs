//! ComfyUI TTS backend.
//!
//! Submits a text prompt to a running ComfyUI instance, opens a WebSocket, and
//! streams audio chunks back from the workflow's TTS node. Chunks are decoded
//! via Symphonia and handed to the shared [`Sink`] which owns resampling and
//! the ring-buffer push.
//!
//! For workflows that don't stream over the socket (e.g. anything ending in
//! `SaveAudio`) we fall back to fetching the output files listed in
//! `/history`.

use anyhow::{Context as _, Result, anyhow};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::io::Cursor;

use crate::workflow;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::{AudioChunk, Sink};

pub struct Backend {
    settings: crate::settings::Shared,
    http: reqwest::Client,
}

impl Backend {
    pub fn new(settings: crate::settings::Shared) -> Self {
        Self {
            settings,
            http: reqwest::Client::new(),
        }
    }

    pub async fn synthesize(&mut self, text: &str, sink: &mut Sink<'_>) -> Result<()> {
        let (base, prepared) = {
            let s = self.settings.read().await;
            (
                s.comfyui_base.clone(),
                workflow::substitute_text(&s.workflow_json, text),
            )
        };
        generate_and_push(&self.http, &base, &prepared, sink).await
    }
}

/// Submit a warmup prompt using the given workflow and wait for ComfyUI to
/// report completion. Discards any audio the WebSocket produces. Intended for
/// nudging the server to load the TTS model into VRAM before real requests.
pub async fn warmup(comfyui_base: &str, workflow_json: &Value) -> Result<()> {
    let http = reqwest::Client::new();
    let prepared = workflow::substitute_text(workflow_json, "warmup");
    let client_id = Uuid::new_v4().to_string();

    let prompt_id = submit_prompt(&http, comfyui_base, &prepared, &client_id).await?;
    let ws_url = ws_url(comfyui_base, &client_id);
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .with_context(|| format!("connecting to {ws_url}"))?;

    while let Some(msg) = ws.next().await {
        let msg = msg.context("ws recv")?;
        if let Message::Text(txt) = msg {
            if is_terminal_for(&txt, &prompt_id)? {
                return Ok(());
            }
        }
    }
    Ok(())
}

async fn generate_and_push(
    http: &reqwest::Client,
    comfyui_base: &str,
    prepared_workflow: &Value,
    sink: &mut Sink<'_>,
) -> Result<()> {
    let client_id = Uuid::new_v4().to_string();

    let prompt_id = submit_prompt(http, comfyui_base, prepared_workflow, &client_id).await?;
    debug!(%prompt_id, %client_id, "prompt accepted by ComfyUI");

    let ws_url = ws_url(comfyui_base, &client_id);
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .with_context(|| format!("connecting to {ws_url}"))?;

    let frames_before = sink.frames_pushed;

    while let Some(msg) = ws.next().await {
        let msg = msg.context("ws recv")?;
        match msg {
            Message::Binary(bytes) => match decode_audio_chunk(&bytes) {
                Ok(chunk) => sink.push(chunk).await?,
                Err(err) => warn!(?err, "failed to decode audio chunk"),
            },
            Message::Text(txt) => {
                if is_terminal_for(&txt, &prompt_id)? {
                    debug!("prompt execution complete");
                    break;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    // Fallback: some workflows (e.g. anything ending in PreviewAudio/SaveAudio)
    // don't stream audio over WS at all — they just write files. If nothing
    // arrived on the socket, walk /history for output files and fetch them.
    if sink.frames_pushed == frames_before {
        debug!("no WS audio; falling back to /history + /view");
        let files = fetch_history_audio_files(http, comfyui_base, &prompt_id).await?;
        for (label, bytes) in files {
            match decode_audio_chunk(&bytes) {
                Ok(chunk) => sink.push(chunk).await?,
                Err(err) => warn!(file = %label, ?err, "failed to decode fetched audio"),
            }
        }
    }

    Ok(())
}

/// Return `(display_label, bytes)` for every audio output file listed under
/// the prompt in `/history`.
async fn fetch_history_audio_files(
    http: &reqwest::Client,
    comfyui_base: &str,
    prompt_id: &str,
) -> Result<Vec<(String, Vec<u8>)>> {
    let base = comfyui_base.trim_end_matches('/');
    let history_url = format!("{base}/history/{prompt_id}");
    let history: Value = http
        .get(&history_url)
        .send()
        .await
        .with_context(|| format!("GET {history_url}"))?
        .error_for_status()?
        .json()
        .await?;

    // history is `{ "<prompt_id>": { "outputs": { "<node_id>": { "audio": [ {filename, subfolder, type}, ... ] } } } }`
    let Some(entry) = history.get(prompt_id) else {
        return Ok(vec![]);
    };
    let Some(outputs) = entry.get("outputs").and_then(|v| v.as_object()) else {
        return Ok(vec![]);
    };

    let mut out = Vec::new();
    for (node_id, node_out) in outputs {
        let Some(audio_arr) = node_out.get("audio").and_then(|v| v.as_array()) else {
            continue;
        };
        for entry in audio_arr {
            let Some(fname) = entry.get("filename").and_then(|v| v.as_str()) else {
                continue;
            };
            let subfolder = entry
                .get("subfolder")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let ftype = entry.get("type").and_then(|v| v.as_str()).unwrap_or("output");
            let view_url = format!("{base}/view");
            let bytes = http
                .get(&view_url)
                .query(&[
                    ("filename", fname),
                    ("subfolder", subfolder),
                    ("type", ftype),
                ])
                .send()
                .await
                .with_context(|| format!("GET {view_url} for {fname}"))?
                .error_for_status()?
                .bytes()
                .await?
                .to_vec();
            info!(node = %node_id, file = %fname, bytes = bytes.len(), "fetched audio output");
            out.push((format!("{node_id}/{fname}"), bytes));
        }
    }
    Ok(out)
}

pub(crate) fn decode_audio_chunk(bytes: &[u8]) -> Result<AudioChunk> {
    let cursor = Cursor::new(bytes.to_vec());
    let mss = MediaSourceStream::new(Box::new(cursor), Default::default());
    let probed = symphonia::default::get_probe().format(
        &Hint::new(),
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| anyhow!("no default track"))?;
    let track_id = track.id;
    let sample_rate = track
        .codec_params
        .sample_rate
        .ok_or_else(|| anyhow!("unknown sample rate"))?;
    let channels = track
        .codec_params
        .channels
        .ok_or_else(|| anyhow!("unknown channel layout"))?
        .count();

    let mut decoder =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;
    let mut samples: Vec<f32> = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = decoder.decode(&packet)?;
        let spec = *decoded.spec();
        let mut buf: SampleBuffer<f32> = SampleBuffer::new(decoded.capacity() as u64, spec);
        buf.copy_interleaved_ref(decoded);

        // Downmix to mono by averaging channels.
        if channels == 1 {
            samples.extend_from_slice(buf.samples());
        } else {
            let frames = buf.samples().chunks_exact(channels);
            samples.reserve(frames.len());
            for frame in frames {
                let sum: f32 = frame.iter().sum();
                samples.push(sum / channels as f32);
            }
        }
    }

    Ok(AudioChunk {
        samples,
        sample_rate,
    })
}

// -- ComfyUI protocol helpers -------------------------------------------------

fn ws_url(comfyui_base: &str, client_id: &str) -> String {
    let base = comfyui_base
        .trim_end_matches('/')
        .replacen("http://", "ws://", 1)
        .replacen("https://", "wss://", 1);
    format!("{base}/ws?clientId={client_id}")
}

async fn submit_prompt(
    http: &reqwest::Client,
    comfyui_base: &str,
    prepared_workflow: &Value,
    client_id: &str,
) -> Result<String> {
    let body = json!({
        "client_id": client_id,
        "prompt": prepared_workflow,
    });
    let resp = http
        .post(format!("{}/prompt", comfyui_base.trim_end_matches('/')))
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!("ComfyUI /prompt {}: {}", status, summarise(&body)));
    }
    let json: Value = serde_json::from_str(&body)
        .with_context(|| format!("parsing ComfyUI /prompt response: {}", summarise(&body)))?;
    let id = json
        .get("prompt_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("no prompt_id in ComfyUI response: {json}"))?;
    Ok(id.to_string())
}

/// Squash a JSON error body to a compact single-line summary. ComfyUI likes to
/// nest details inside `node_errors[node_id].errors[].message`; if that's
/// present, extract those lines instead of dumping the whole tree.
fn summarise(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        let mut lines: Vec<String> = Vec::new();
        if let Some(node_errors) = v.get("node_errors").and_then(|n| n.as_object()) {
            for (node_id, ne) in node_errors {
                if let Some(errs) = ne.get("errors").and_then(|e| e.as_array()) {
                    for err in errs {
                        if let Some(m) = err.get("message").and_then(|s| s.as_str()) {
                            lines.push(format!("node {node_id}: {m}"));
                        }
                    }
                }
            }
        }
        if let Some(err) = v.get("error") {
            if let Some(msg) = err.get("message").and_then(|s| s.as_str()) {
                lines.push(msg.to_string());
            } else if let Some(s) = err.as_str() {
                lines.push(s.to_string());
            }
        }
        if !lines.is_empty() {
            return lines.join(" | ");
        }
    }
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

/// Returns `Ok(true)` if the given text frame indicates our prompt finished
/// successfully; propagates `Err(...)` for execution_error frames matching us.
fn is_terminal_for(txt: &str, prompt_id: &str) -> Result<bool> {
    let Ok(v) = serde_json::from_str::<Value>(txt) else {
        return Ok(false);
    };
    let ty = v.get("type").and_then(|s| s.as_str()).unwrap_or("");
    let matches_id = v
        .get("data")
        .and_then(|d| d.get("prompt_id"))
        .and_then(|p| p.as_str())
        == Some(prompt_id);
    if matches_id {
        if ty == "execution_error" {
            return Err(anyhow!("ComfyUI execution error: {v}"));
        }
        if ty == "executed" || ty == "execution_success" || ty == "execution_end" {
            return Ok(true);
        }
    }
    Ok(false)
}
