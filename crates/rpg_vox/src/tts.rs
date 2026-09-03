//! ComfyUI TTS client.
//!
//! Submits a text prompt to a running ComfyUI instance, opens a WebSocket, and
//! streams audio chunks back from a custom TTS node. Chunks are decoded via
//! Symphonia, resampled to the target rate if necessary, and pushed into the
//! shared ring buffer that feeds the PipeWire source.
//!
//! The exact wire format of the custom ComfyUI node isn't finalized. The
//! `ComfyClient` trait below is the seam — swap the impl once the node's
//! protocol is nailed down.

use anyhow::{Context as _, Result, anyhow};
use futures_util::StreamExt;
use rtrb::Producer;
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use serde_json::{Value, json};
use std::io::Cursor;

use crate::workflow;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

pub struct Config {
    pub target_sample_rate: u32,
}

/// One utterance request: the text to speak plus a channel to report the
/// result back to the caller (usually the /say HTTP handler).
pub struct SayRequest {
    pub text: String,
    pub reply: oneshot::Sender<Result<usize, String>>,
}

pub async fn run(
    cfg: Config,
    settings: crate::settings::Shared,
    mut say_rx: mpsc::Receiver<SayRequest>,
    mut producer: Producer<f32>,
) -> Result<()> {
    let http = reqwest::Client::new();

    while let Some(SayRequest { text, reply }) = say_rx.recv().await {
        // Read live settings + build the per-utterance workflow.
        let (base, prepared) = {
            let s = settings.read().await;
            (
                s.comfyui_base.clone(),
                workflow::substitute_text(&s.workflow_json, &text),
            )
        };
        info!(chars = text.len(), "generating speech");
        let result = generate_and_push(&http, &base, &prepared, cfg.target_sample_rate, &mut producer)
            .await
            .map_err(|err| {
                let msg = format!("{err:#}");
                error!(err = %msg, "utterance failed");
                msg
            })
            .inspect(|frames| {
                info!(frames, "utterance delivered to ring buffer");
            });
        let _ = reply.send(result);
    }

    Ok(())
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
    target_rate: u32,
    producer: &mut Producer<f32>,
) -> Result<usize> {
    let client_id = Uuid::new_v4().to_string();

    let prompt_id = submit_prompt(http, comfyui_base, prepared_workflow, &client_id).await?;
    debug!(%prompt_id, %client_id, "prompt accepted by ComfyUI");

    let ws_url = ws_url(comfyui_base, &client_id);
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .with_context(|| format!("connecting to {ws_url}"))?;

    let mut frames_pushed = 0usize;
    let mut incoming_rate: Option<u32> = None;
    let mut resampler: Option<SincFixedIn<f32>> = None;

    while let Some(msg) = ws.next().await {
        let msg = msg.context("ws recv")?;
        match msg {
            Message::Binary(bytes) => {
                // The custom node is expected to emit encoded audio (e.g. a WAV
                // chunk or a raw PCM frame with a small header). Symphonia
                // handles the format probing for us for standard containers.
                match decode_audio_chunk(&bytes) {
                    Ok(chunk) => {
                        if incoming_rate.is_none() {
                            incoming_rate = Some(chunk.sample_rate);
                            if chunk.sample_rate != target_rate {
                                resampler = Some(build_resampler(chunk.sample_rate, target_rate)?);
                            }
                            info!(
                                rate = chunk.sample_rate,
                                target = target_rate,
                                "audio format known"
                            );
                        }
                        let resampled = if let Some(rs) = resampler.as_mut() {
                            resample(rs, &chunk.samples)?
                        } else {
                            chunk.samples
                        };
                        frames_pushed += push_samples(producer, &resampled);
                    }
                    Err(err) => warn!(?err, "failed to decode audio chunk"),
                }
            }
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
    if frames_pushed == 0 {
        debug!("no WS audio; falling back to /history + /view");
        let files = fetch_history_audio_files(http, comfyui_base, &prompt_id).await?;
        for (label, bytes) in files {
            let chunk = match decode_audio_chunk(&bytes) {
                Ok(c) => c,
                Err(err) => {
                    warn!(file = %label, ?err, "failed to decode fetched audio");
                    continue;
                }
            };
            if incoming_rate.is_none() {
                incoming_rate = Some(chunk.sample_rate);
                if chunk.sample_rate != target_rate {
                    resampler = Some(build_resampler(chunk.sample_rate, target_rate)?);
                }
                info!(rate = chunk.sample_rate, target = target_rate, "audio format known");
            }
            let resampled = if let Some(rs) = resampler.as_mut() {
                resample(rs, &chunk.samples)?
            } else {
                chunk.samples
            };
            frames_pushed += push_samples(producer, &resampled);
        }
    }

    Ok(frames_pushed)
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

fn push_samples(producer: &mut Producer<f32>, samples: &[f32]) -> usize {
    let mut written = 0;
    for &s in samples {
        if producer.push(s).is_err() {
            // Ring full — drop the tail rather than block the async task. In
            // practice this only happens if PipeWire has stalled.
            warn!("ring buffer full; dropping remaining samples");
            break;
        }
        written += 1;
    }
    written
}

struct DecodedChunk {
    samples: Vec<f32>,
    sample_rate: u32,
}

fn decode_audio_chunk(bytes: &[u8]) -> Result<DecodedChunk> {
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

    Ok(DecodedChunk {
        samples,
        sample_rate,
    })
}

fn build_resampler(input_rate: u32, output_rate: u32) -> Result<SincFixedIn<f32>> {
    let params = SincInterpolationParameters {
        sinc_len: 128,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Cubic,
        oversampling_factor: 128,
        window: WindowFunction::BlackmanHarris2,
    };
    let ratio = output_rate as f64 / input_rate as f64;
    Ok(SincFixedIn::<f32>::new(ratio, 2.0, params, 1024, 1)?)
}

fn resample(rs: &mut SincFixedIn<f32>, input: &[f32]) -> Result<Vec<f32>> {
    let chunk_size = rs.input_frames_next();
    let mut out = Vec::new();
    for chunk in input.chunks(chunk_size) {
        let mut padded;
        let slice: &[f32] = if chunk.len() == chunk_size {
            chunk
        } else {
            padded = vec![0.0f32; chunk_size];
            padded[..chunk.len()].copy_from_slice(chunk);
            &padded
        };
        let processed = rs.process(&[slice], None)?;
        out.extend_from_slice(&processed[0]);
    }
    Ok(out)
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
        // ComfyUI puts diagnostic detail (missing nodes, validation errors,
        // etc.) in the body — bubble it up so /say returns something useful.
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
        // Try to extract validation error messages first.
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
    // Fallback: whole body, single-line, truncated.
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
