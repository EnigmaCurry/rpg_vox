//! Browser-microphone client pool: /mic.ws → decoded PCM → source callback.
//!
//! Symmetric counterpart to [`crate::monitor`]. Each browser tab opens a
//! WebSocket that carries 20 ms interleaved-stereo Opus frames produced by
//! WebCodecs `AudioEncoder`. We decode each frame here and push its
//! `[L, R]` pairs into a per-slot SPSC ring; the pipewire source callback
//! drains those rings each cycle and sums each active slot's contribution
//! into either the music strip or a specific Vox slot, based on that
//! slot's routing atomic.
//!
//! The pool has a fixed slot count ([`WEB_MIC_SLOTS`]) so the RT callback
//! never allocates. Slots are claimed on connect and released on
//! disconnect via [`ClaimedSlot`]'s Drop. The routing preference for each
//! client is keyed by client UUID and persisted (see
//! [`crate::store::Store::put_web_mic_routings`]) so a reconnection
//! restores the last-picked destination automatically.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU8, Ordering},
};

use axum::{
    extract::{
        Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use rtrb::{Consumer, Producer, RingBuffer};
use serde::Deserialize;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::http::AppState;

/// Concurrent web-mic client cap. Each slot costs one SPSC ring (~half a
/// second at 48 kHz stereo = ~192 KiB) plus a handful of atomics. Four
/// covers the common single-user + a couple of tabs / collaborators
/// scenario; the RT callback iterates them linearly, so higher counts
/// scale fine as long as memory allows.
pub const WEB_MIC_SLOTS: usize = 4;

/// Encoded routing target held in each slot's [`AtomicU8`]. The RT
/// source callback reads this every process cycle and routes the slot's
/// PCM accordingly:
///   0             — Off (drop samples).
///   1             — Music strip.
///   2 + n         — Vox slot `n` (0-based).
pub const TARGET_OFF: u8 = 0;
pub const TARGET_MUSIC: u8 = 1;
pub const TARGET_VOX_BASE: u8 = 2;

/// Convenience view onto a sink target, decoupled from `SinkRole` in
/// `pw_source` so this module doesn't pull in pipewire types. Route wire
/// values use the same suffixes as pipewire device routings ("music",
/// "vox", "vox2", …) for symmetry with the existing UI.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WebMicTarget {
    Music,
    Vox(usize),
}

impl WebMicTarget {
    fn encode(self) -> u8 {
        match self {
            Self::Music => TARGET_MUSIC,
            Self::Vox(i) => TARGET_VOX_BASE.saturating_add(i as u8),
        }
    }
    /// URL/JSON selector — identical to `pw_source::SinkRole::suffix()`
    /// so the frontend uses the same routing strings for web-mic and
    /// pipewire sources.
    pub fn suffix(self) -> String {
        match self {
            Self::Music => "music".to_string(),
            Self::Vox(0) => "vox".to_string(),
            Self::Vox(i) => format!("vox{}", i + 1),
        }
    }
    pub fn from_suffix(s: &str) -> Option<Self> {
        if s == "music" {
            return Some(Self::Music);
        }
        if s == "vox" {
            return Some(Self::Vox(0));
        }
        if let Some(rest) = s.strip_prefix("vox") {
            if let Ok(n) = rest.parse::<usize>() {
                if n >= 1 {
                    return Some(Self::Vox(n - 1));
                }
            }
        }
        None
    }
}

/// Shared slot state. `active` and `target` are RT-safe atomics read by
/// the pipewire source callback; `client_uuid` and `producer` are only
/// touched from the tokio side (claim/release paths).
pub struct WebMicSlotState {
    active: AtomicBool,
    target: AtomicU8,
    /// Client-reported mute state. The client zeros out the encoded
    /// PCM before sending, so audio flow is unchanged — this atomic
    /// exists purely so `/pw/graph` can distinguish "muted" from "just
    /// silent" on the Sources row. Set via a `{"muted": bool}` text
    /// WebSocket message from the browser.
    muted: AtomicBool,
    client_uuid: Mutex<Option<Uuid>>,
    /// Slot's SPSC producer, taken by whichever WS task claims the slot
    /// and returned to this Mutex by [`ClaimedSlot`]'s Drop. `None`
    /// while a client owns it.
    producer: Mutex<Option<Producer<[f32; 2]>>>,
}

impl WebMicSlotState {
    #[inline]
    pub fn active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }
    #[inline]
    pub fn target_raw(&self) -> u8 {
        self.target.load(Ordering::Relaxed)
    }
    /// Decoded routing target, or `None` when the slot is set to Off.
    pub fn target(&self) -> Option<WebMicTarget> {
        match self.target_raw() {
            TARGET_OFF => None,
            TARGET_MUSIC => Some(WebMicTarget::Music),
            n => Some(WebMicTarget::Vox((n - TARGET_VOX_BASE) as usize)),
        }
    }
    pub fn set_target(&self, target: Option<WebMicTarget>) {
        let raw = target.map(WebMicTarget::encode).unwrap_or(TARGET_OFF);
        self.target.store(raw, Ordering::Relaxed);
    }
    pub fn client_uuid(&self) -> Option<Uuid> {
        *self.client_uuid.lock().expect("web_mic uuid mutex poisoned")
    }
    #[inline]
    pub fn muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }
    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }
}

/// Per-slot handle handed to the pipewire source callback. Bundles the
/// SPSC consumer with the shared state so the callback can pop the ring
/// and route the samples in one place.
pub struct WebMicRtSlot {
    pub consumer: Consumer<[f32; 2]>,
    pub state: Arc<WebMicSlotState>,
}

/// The pool itself. `Arc<Self>` is cheap to clone; the slots inside are
/// each behind their own `Arc` too so the RT thread and HTTP handlers can
/// reference the same state without holding the pool.
pub struct WebMicPool {
    slots: Vec<Arc<WebMicSlotState>>,
}

/// Failure modes for a slot claim. Both are recoverable at the caller
/// level — the WS handler closes the socket with a matching close code.
#[derive(Debug)]
pub enum ClaimError {
    /// Every slot is already assigned to some other client.
    Full,
    /// A previous connection with the same uuid still holds the slot's
    /// producer. Usually resolves within one round-trip once the stale
    /// task's drop runs; the client should just retry.
    Busy,
}

impl WebMicPool {
    /// Construct the pool. Returns the pool plus a `Vec<WebMicRtSlot>` to
    /// hand to the pipewire source callback; each slot's Producer stays
    /// inside the pool until claimed by a client. `ring_frames` sizes
    /// each SPSC ring — half a second at the target rate is enough to
    /// absorb decode/RT jitter without piling perceivable latency into
    /// the mic feed.
    pub fn new(ring_frames: usize) -> (Arc<Self>, Vec<WebMicRtSlot>) {
        let mut slots: Vec<Arc<WebMicSlotState>> = Vec::with_capacity(WEB_MIC_SLOTS);
        let mut rt_slots: Vec<WebMicRtSlot> = Vec::with_capacity(WEB_MIC_SLOTS);
        for _ in 0..WEB_MIC_SLOTS {
            let (producer, consumer) = RingBuffer::<[f32; 2]>::new(ring_frames);
            let state = Arc::new(WebMicSlotState {
                active: AtomicBool::new(false),
                target: AtomicU8::new(TARGET_OFF),
                muted: AtomicBool::new(false),
                client_uuid: Mutex::new(None),
                producer: Mutex::new(Some(producer)),
            });
            rt_slots.push(WebMicRtSlot {
                consumer,
                state: state.clone(),
            });
            slots.push(state);
        }
        (Arc::new(Self { slots }), rt_slots)
    }

    /// Iterate every slot's shared state. HTTP handlers walk this to
    /// enumerate currently-active web-mic clients for the Sources block.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<WebMicSlotState>> {
        self.slots.iter()
    }

    /// Return the slot currently claimed by `uuid`, if any.
    pub fn find_by_uuid(&self, uuid: &Uuid) -> Option<Arc<WebMicSlotState>> {
        for s in &self.slots {
            if s.client_uuid.lock().expect("web_mic uuid mutex poisoned").as_ref() == Some(uuid) {
                return Some(s.clone());
            }
        }
        None
    }

    /// Claim a free slot for this uuid. Returns a [`ClaimedSlot`] whose
    /// Drop returns the slot to the pool. If `initial_target` is
    /// provided (persisted pref restored from disk), the slot's atomic
    /// is seeded before the WS task starts pushing PCM.
    pub fn claim(
        &self,
        uuid: Uuid,
        initial_target: Option<WebMicTarget>,
    ) -> Result<ClaimedSlot, ClaimError> {
        // Reconnect path: if some slot still carries this uuid (a
        // previous WS task's drop hasn't run yet, or its producer is
        // currently in the Mutex because a stale one closed the socket
        // uncleanly), reuse it so we don't leak the slot.
        for s in &self.slots {
            let mut u = s.client_uuid.lock().expect("web_mic uuid mutex poisoned");
            if u.as_ref() == Some(&uuid) {
                let mut p = s.producer.lock().expect("web_mic producer mutex poisoned");
                let Some(producer) = p.take() else {
                    return Err(ClaimError::Busy);
                };
                s.active.store(true, Ordering::Relaxed);
                *u = Some(uuid);
                s.set_target(initial_target);
                return Ok(ClaimedSlot {
                    state: s.clone(),
                    producer: Some(producer),
                });
            }
        }
        // Fresh claim: first slot with no uuid AND a producer available.
        for s in &self.slots {
            let mut u = s.client_uuid.lock().expect("web_mic uuid mutex poisoned");
            if u.is_some() {
                continue;
            }
            let mut p = s.producer.lock().expect("web_mic producer mutex poisoned");
            let Some(producer) = p.take() else {
                continue;
            };
            *u = Some(uuid);
            s.active.store(true, Ordering::Relaxed);
            s.set_target(initial_target);
            return Ok(ClaimedSlot {
                state: s.clone(),
                producer: Some(producer),
            });
        }
        Err(ClaimError::Full)
    }
}

/// RAII slot claim. Push decoded stereo pairs via [`Self::push_frame`];
/// drop returns the producer to the pool and marks the slot free.
pub struct ClaimedSlot {
    state: Arc<WebMicSlotState>,
    producer: Option<Producer<[f32; 2]>>,
}

impl ClaimedSlot {
    pub fn state(&self) -> &Arc<WebMicSlotState> {
        &self.state
    }
    /// Push one interleaved-stereo frame. Returns `Err` when the ring
    /// overflowed — the caller should count that as an underrun on the
    /// server side (source callback couldn't drain fast enough).
    pub fn push_frame(
        &mut self,
        frame: [f32; 2],
    ) -> Result<(), rtrb::PushError<[f32; 2]>> {
        self.producer
            .as_mut()
            .expect("producer taken twice")
            .push(frame)
    }
}

impl Drop for ClaimedSlot {
    fn drop(&mut self) {
        if let Some(p) = self.producer.take() {
            *self.state.producer.lock().expect("web_mic producer mutex poisoned") = Some(p);
        }
        self.state.active.store(false, Ordering::Relaxed);
        self.state.target.store(TARGET_OFF, Ordering::Relaxed);
        self.state.muted.store(false, Ordering::Relaxed);
        *self.state.client_uuid.lock().expect("web_mic uuid mutex poisoned") = None;
    }
}

// -----------------------------------------------------------------------------
// HTTP: /mic.ws WebSocket handler
// -----------------------------------------------------------------------------

/// Max Opus frame at 48 kHz stereo — 120 ms per channel × 2 channels
/// interleaved. WebCodecs typically emits 20 ms frames but this leaves
/// room for the largest supported packet.
const MAX_DECODED_FRAMES: usize = 5760 * 2;

/// Query string on `/mic.ws?client=<uuid>`. The uuid is a stable
/// per-browser identifier (v4, hyphenated) — the frontend generates it
/// on first load and stores it in localStorage so a page reload keeps
/// its slot + routing pref.
#[derive(Debug, Deserialize)]
pub struct WebMicQuery {
    pub client: String,
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(q): Query<WebMicQuery>,
    State(state): State<AppState>,
) -> Response {
    let uuid = match Uuid::parse_str(&q.client) {
        Ok(u) => u,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, "invalid client uuid").into_response();
        }
    };
    // Seed the slot's target atomic from the persisted routing pref for
    // this uuid, if any. A returning client that had picked Vox 1 last
    // session lands back on Vox 1 as soon as the WS opens — no need for
    // the browser to re-select before its audio flows.
    let initial_target = {
        let map = state
            .web_mic_routings
            .lock()
            .expect("web_mic_routings mutex poisoned");
        map.get(&q.client)
            .and_then(|s| WebMicTarget::from_suffix(s))
    };
    let claimed = match state.web_mic_pool.claim(uuid, initial_target) {
        Ok(c) => c,
        Err(ClaimError::Full) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "web-mic pool is full — no free slots",
            )
                .into_response();
        }
        Err(ClaimError::Busy) => {
            return (
                StatusCode::CONFLICT,
                "another connection is still active for this client uuid",
            )
                .into_response();
        }
    };
    ws.on_upgrade(move |socket| async move {
        if let Err(err) = run_subscriber(socket, claimed).await {
            debug!(?err, "web-mic subscriber ended");
        }
    })
}

async fn run_subscriber(mut socket: WebSocket, mut claimed: ClaimedSlot) -> anyhow::Result<()> {
    let mut decoder = opus::Decoder::new(48_000, opus::Channels::Stereo)?;
    // Reused decode target — Opus emits at most 120 ms of stereo audio
    // per packet, so this buffer never needs to grow.
    let mut pcm_buf: Vec<f32> = vec![0.0; MAX_DECODED_FRAMES];
    let uuid_str = claimed
        .state()
        .client_uuid()
        .map(|u| u.to_string())
        .unwrap_or_default();
    info!(client = %uuid_str, "web-mic subscriber connected");
    let mut overflow_frames: u64 = 0;
    let mut decode_failures: u64 = 0;
    loop {
        let msg = socket.recv().await;
        match msg {
            Some(Ok(Message::Binary(data))) => {
                match decoder.decode_float(&data, &mut pcm_buf, false) {
                    Ok(frames_per_channel) => {
                        for i in 0..frames_per_channel {
                            let l = pcm_buf[i * 2];
                            let r = pcm_buf[i * 2 + 1];
                            if claimed.push_frame([l, r]).is_err() {
                                overflow_frames = overflow_frames.saturating_add(1);
                                if overflow_frames.is_power_of_two() {
                                    warn!(
                                        client = %uuid_str,
                                        dropped = overflow_frames,
                                        "web-mic ring overflow — RT drainer behind?"
                                    );
                                }
                            }
                        }
                    }
                    Err(err) => {
                        // Power-of-two backoff so a client that sends
                        // garbage or a mismatched codec doesn't fill
                        // the log at ~50 Hz.
                        decode_failures = decode_failures.saturating_add(1);
                        if decode_failures.is_power_of_two() {
                            warn!(
                                ?err,
                                client = %uuid_str,
                                total_failed = decode_failures,
                                "web-mic decode failed; ignoring frame"
                            );
                        }
                    }
                }
            }
            Some(Ok(Message::Close(_))) | None => {
                info!(client = %uuid_str, "web-mic subscriber closed socket");
                return Ok(());
            }
            Some(Ok(Message::Ping(payload))) => {
                let _ = socket.send(Message::Pong(payload)).await;
            }
            Some(Ok(Message::Text(txt))) => {
                // Client-side mute signal: `{"muted": true|false}`.
                // Cheap to parse — only fires on state changes plus
                // once on WS open. Malformed messages are ignored so
                // a future protocol extension can add fields without
                // needing a schema handshake.
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
                    if let Some(m) = v.get("muted").and_then(|x| x.as_bool()) {
                        claimed.state().set_muted(m);
                    }
                }
            }
            Some(Ok(_)) => {} // ignore Pong / other unknown binary shapes
            Some(Err(err)) => {
                warn!(?err, client = %uuid_str, "web-mic socket recv error");
                return Ok(());
            }
        }
    }
}


