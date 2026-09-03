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
use serde_json::json;
use std::io::Cursor;
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
        // Re-read settings each utterance so live edits take effect immediately.
        let base = settings.read().await.comfyui_base.clone();
        let client = ComfyClientImpl::new(base);
        info!(chars = text.len(), "generating speech");
        let result =
            generate_and_push(&http, &client, &text, cfg.target_sample_rate, &mut producer)
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

async fn generate_and_push(
    http: &reqwest::Client,
    client: &ComfyClientImpl,
    text: &str,
    target_rate: u32,
    producer: &mut Producer<f32>,
) -> Result<usize> {
    let client_id = Uuid::new_v4().to_string();

    // 1. Submit workflow. `submit_prompt` builds the JSON body for the user's
    //    workflow — it currently sends a placeholder; slot in the real workflow
    //    JSON once the custom node's schema is confirmed.
    let prompt_id = client.submit_prompt(http, text, &client_id).await?;
    debug!(%prompt_id, %client_id, "prompt accepted by ComfyUI");

    // 2. Open the ComfyUI WebSocket with the same client_id and consume audio
    //    frames until the prompt completes.
    let ws_url = client.ws_url(&client_id);
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
                // ComfyUI ships JSON status frames on the same socket.
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
                    let ty = v.get("type").and_then(|s| s.as_str()).unwrap_or("");
                    if ty == "executed" || ty == "execution_success" || ty == "execution_end" {
                        if v.get("data")
                            .and_then(|d| d.get("prompt_id"))
                            .and_then(|p| p.as_str())
                            == Some(&prompt_id)
                        {
                            debug!("prompt execution complete");
                            break;
                        }
                    } else if ty == "execution_error" {
                        return Err(anyhow!("ComfyUI execution error: {v}"));
                    }
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    Ok(frames_pushed)
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

// -- ComfyUI client -----------------------------------------------------------

struct ComfyClientImpl {
    base: String,
}

impl ComfyClientImpl {
    fn new(base: String) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
        }
    }

    fn ws_url(&self, client_id: &str) -> String {
        let ws_base = self
            .base
            .replacen("http://", "ws://", 1)
            .replacen("https://", "wss://", 1);
        format!("{ws_base}/ws?clientId={client_id}")
    }

    /// Submit a prompt to `POST /prompt`. Returns the prompt_id.
    ///
    /// The `workflow` payload here is a placeholder — replace with the real
    /// workflow JSON for the ComfyUI TTS custom node once its schema is known.
    async fn submit_prompt(
        &self,
        http: &reqwest::Client,
        text: &str,
        client_id: &str,
    ) -> Result<String> {
        let body = json!({
            "client_id": client_id,
            "prompt": {
                // TODO: replace with the real workflow graph. This shape is what
                // ComfyUI expects: a map of node_id -> { class_type, inputs }.
                "1": {
                    "class_type": "TTS_Stream_PLACEHOLDER",
                    "inputs": { "text": text }
                }
            }
        });
        let resp = http
            .post(format!("{}/prompt", self.base))
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        let json: serde_json::Value = resp.json().await?;
        let id = json
            .get("prompt_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("no prompt_id in ComfyUI response: {json}"))?;
        Ok(id.to_string())
    }
}
