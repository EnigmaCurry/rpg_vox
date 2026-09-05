//! Browser-monitor audio tap.
//!
//! The TTS runner publishes the same resampled mono f32 PCM it hands to the
//! pipewire ring buffer through a [`broadcast::Sender`]. This module fans
//! that stream out to `/monitor.ws` WebSocket subscribers as Opus frames,
//! one encoder per client (Opus encoders are stateful, and per-client state
//! costs ~a percent of a CPU core each even under heavy load).
//!
//! Slow subscribers drop-on-lag: `broadcast::Receiver::recv` returns
//! `Lagged(n)` when a client falls behind, at which point we reset the
//! encoder state and keep going. Live monitor semantics — a lagging tab
//! skips forward instead of stalling the rest of the fan-out.
//!
//! The tap fires strictly for PCM that reaches the pipewire mic — see
//! `Sink::push` in `tts/mod.rs`. Synthesis previews (`/widgets` POST/PUT
//! and `/synthesize`) never reach a monitor listener because that would
//! diverge from what Discord hears.

use std::sync::Arc;

use axum::{
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::Response,
};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::http::AppState;

/// One 20 ms frame at 48 kHz mono is the sweet spot for Opus latency vs.
/// packetization overhead, and matches what most WebRTC stacks use. If we
/// ever change the pipewire target rate this needs to move in step.
const FRAME_SAMPLES_48K: usize = 960;

/// Broadcast channel capacity in messages. Each message is one PCM chunk
/// (whatever size the TTS backend emits), so this is a slot count, not a
/// byte cap. 32 gives a slow subscriber a couple of seconds of grace before
/// it starts skipping — beyond that they'd have drifted so far behind that
/// silently dropping is the right call anyway.
pub const BROADCAST_CAPACITY: usize = 32;

/// PCM chunk shared over the broadcast tap. `Arc<[f32]>` lets the runner
/// publish one allocation that every subscriber reads without a copy.
pub type PcmChunk = Arc<[f32]>;

/// Create the broadcast pair shared by the tts runner (producer side) and
/// every `/monitor.ws` subscriber (receiver side).
pub fn channel() -> (broadcast::Sender<PcmChunk>, broadcast::Receiver<PcmChunk>) {
    broadcast::channel(BROADCAST_CAPACITY)
}

pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    let rx = state.monitor_tap.subscribe();
    let sample_rate = state.monitor_sample_rate;
    ws.on_upgrade(move |socket| async move {
        if let Err(err) = run_subscriber(socket, rx, sample_rate).await {
            debug!(?err, "monitor subscriber ended");
        }
    })
}

async fn run_subscriber(
    mut socket: WebSocket,
    mut rx: broadcast::Receiver<Arc<[f32]>>,
    sample_rate: u32,
) -> anyhow::Result<()> {
    if sample_rate != 48_000 {
        // Opus supports 8/12/16/24/48 kHz. Everything in rpg_vox targets 48k
        // (see main.rs default + pipewire config) so bail cleanly if someone
        // reconfigures to a rate we don't handle rather than silently sending
        // garbled audio.
        let _ = socket
            .send(Message::Close(Some(axum::extract::ws::CloseFrame {
                code: 1011,
                reason: format!("unsupported sample rate {sample_rate}").into(),
            })))
            .await;
        anyhow::bail!("monitor requires 48 kHz PCM; got {sample_rate}");
    }

    let mut encoder = opus::Encoder::new(
        48_000,
        opus::Channels::Mono,
        opus::Application::Audio,
    )?;
    // A little inband FEC helps the browser recover from single-packet loss
    // over the WebSocket. Cheap on the encoder side.
    let _ = encoder.set_inband_fec(true);
    // 64 kbit/s is more than enough for mono speech; keeps latency low
    // relative to VBR at the same average bitrate.
    let _ = encoder.set_bitrate(opus::Bitrate::Bits(64_000));

    // Rolling PCM buffer — the TTS runner sends chunks of arbitrary length,
    // we slice them into fixed-size Opus frames here.
    let mut pcm_buf: Vec<f32> = Vec::with_capacity(FRAME_SAMPLES_48K * 4);
    // Reused encoder output buffer; 4000 bytes is opus's documented max per
    // frame at any supported bitrate.
    let mut opus_out: Vec<u8> = vec![0u8; 4000];

    info!("monitor subscriber connected");

    loop {
        tokio::select! {
            // Drain any client-side messages (mostly ping/pong / close). We
            // don't accept any control traffic yet — just keep the socket
            // healthy and notice close events.
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => {
                        info!("monitor subscriber closed socket");
                        return Ok(());
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        // axum auto-responds to Ping, but be defensive.
                        let _ = socket.send(Message::Pong(payload)).await;
                    }
                    Some(Ok(_)) => {} // ignore text / binary / pong from client
                    Some(Err(err)) => {
                        warn!(?err, "monitor socket recv error");
                        return Ok(());
                    }
                }
            }
            recv = rx.recv() => {
                match recv {
                    Ok(chunk) => {
                        pcm_buf.extend_from_slice(&chunk);
                        // Slice out as many full Opus frames as we've buffered.
                        while pcm_buf.len() >= FRAME_SAMPLES_48K {
                            // Encode the head frame in place, then rotate the
                            // remainder to the front. `drain` avoids realloc.
                            let n = encoder.encode_float(
                                &pcm_buf[..FRAME_SAMPLES_48K],
                                &mut opus_out,
                            )?;
                            pcm_buf.drain(..FRAME_SAMPLES_48K);
                            if socket
                                .send(Message::Binary(opus_out[..n].to_vec()))
                                .await
                                .is_err()
                            {
                                info!("monitor subscriber send failed; closing");
                                return Ok(());
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // Drop stale PCM and reset the encoder — the browser
                        // decoder will resync on the next few frames. This is
                        // the whole point of using broadcast for a live tap.
                        warn!(dropped = n, "monitor subscriber lagged; resetting encoder");
                        pcm_buf.clear();
                        encoder = opus::Encoder::new(
                            48_000,
                            opus::Channels::Mono,
                            opus::Application::Audio,
                        )?;
                        let _ = encoder.set_inband_fec(true);
                        let _ = encoder.set_bitrate(opus::Bitrate::Bits(64_000));
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        info!("monitor tap closed; ending subscriber");
                        return Ok(());
                    }
                }
            }
        }
    }
}
