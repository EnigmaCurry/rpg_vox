//! Runtime mixer state for the pipewire source.
//!
//! Three input strips (TTS, music-sink, vox-sink) plus a master. Each strip
//! carries a linear gain (0..2), a constant-power pan (-1..+1) and a mute
//! flag; the master carries gain + mute only. Values are read from the
//! realtime source callback, so state lives in atomics — `Arc<AtomicMixer>`
//! is cheap to clone, lock-free to read, and lock-free to update from HTTP.
//!
//! Persisted through the existing sqlite `app_state` KV table (see
//! [`crate::store::Store::get_mixer`]/`put_mixer`). Loaded once at startup
//! and re-applied whenever the HTTP endpoint accepts a patch.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

const RELAXED: Ordering = Ordering::Relaxed;

/// Upper limit for a linear gain fader (approx +6 dB). Keeping the ceiling
/// modest here means the constant-power pan curve never turns a nominal
/// unity signal into hard-clipping output on its own.
pub const MAX_GAIN: f32 = 2.0;

fn clamp_gain(v: f32) -> f32 {
    v.clamp(0.0, MAX_GAIN)
}
fn clamp_pan(v: f32) -> f32 {
    v.clamp(-1.0, 1.0)
}

fn store_f32(cell: &AtomicU32, v: f32) {
    cell.store(v.to_bits(), RELAXED);
}
fn load_f32(cell: &AtomicU32) -> f32 {
    f32::from_bits(cell.load(RELAXED))
}

/// Atomic RT-safe view of one input strip.
#[derive(Debug)]
pub struct AtomicChannel {
    gain: AtomicU32,
    pan: AtomicU32,
    mute: AtomicBool,
    /// Peak sample magnitude seen since the last `take_peak`. Written by
    /// the source callback via `AtomicU32::fetch_max`; readers use
    /// `swap(0)` to atomically fetch-and-reset.  Stored as the bit
    /// representation of a non-negative `f32` so `fetch_max` on `u32`
    /// bits is the same as `max` on the underlying magnitudes (positive
    /// IEEE-754 floats sort the same in u32 order).
    peak: AtomicU32,
}

impl AtomicChannel {
    fn new(state: ChannelState) -> Self {
        Self {
            gain: AtomicU32::new(clamp_gain(state.gain).to_bits()),
            pan: AtomicU32::new(clamp_pan(state.pan).to_bits()),
            mute: AtomicBool::new(state.mute),
            peak: AtomicU32::new(0),
        }
    }

    /// Update the recent peak with a non-negative magnitude (typically
    /// `max(|l|, |r|)` for the strip's stereo pair). Called from the RT
    /// source callback once per process cycle, not once per frame.
    #[inline]
    pub fn observe_peak(&self, magnitude: f32) {
        if !magnitude.is_finite() || magnitude <= 0.0 {
            return;
        }
        self.peak.fetch_max(magnitude.to_bits(), RELAXED);
    }

    /// Atomically read + reset the recent peak. Returns the max magnitude
    /// seen since the previous call (or 0 if the callback hasn't run).
    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, RELAXED))
    }

    /// Compute the (left, right) linear multipliers for a mono sample given
    /// the current gain/pan/mute. Constant-power pan law so a centred signal
    /// costs 0.707 in each channel rather than doubling to +6 dB.
    #[inline]
    pub fn stereo_gains(&self) -> (f32, f32) {
        if self.mute.load(RELAXED) {
            return (0.0, 0.0);
        }
        let g = load_f32(&self.gain);
        let pan = load_f32(&self.pan).clamp(-1.0, 1.0);
        // pan ∈ [-1, +1] → angle ∈ [0, π/2]
        let angle = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
        let l = angle.cos() * g;
        let r = angle.sin() * g;
        (l, r)
    }

    pub fn snapshot(&self) -> ChannelState {
        ChannelState {
            gain: load_f32(&self.gain),
            pan: load_f32(&self.pan),
            mute: self.mute.load(RELAXED),
        }
    }

    fn apply(&self, patch: ChannelPatch) {
        if let Some(g) = patch.gain {
            store_f32(&self.gain, clamp_gain(g));
        }
        if let Some(p) = patch.pan {
            store_f32(&self.pan, clamp_pan(p));
        }
        if let Some(m) = patch.mute {
            self.mute.store(m, RELAXED);
        }
    }
}

/// Full mixer state. `Arc<Self>` is handed to the pipewire source thread,
/// the HTTP layer and the persistence layer — clones are cheap.
#[derive(Debug)]
pub struct AtomicMixer {
    pub tts: AtomicChannel,
    pub music: AtomicChannel,
    pub vox: AtomicChannel,
    master_gain: AtomicU32,
    master_mute: AtomicBool,
    /// Master output peaks per side (post-master-gain, post-clip). Same
    /// fetch_max / swap-to-reset semantics as `AtomicChannel::peak`.
    master_peak_l: AtomicU32,
    master_peak_r: AtomicU32,
}

impl AtomicMixer {
    pub fn new(state: MixerState) -> Arc<Self> {
        Arc::new(Self {
            tts: AtomicChannel::new(state.tts),
            music: AtomicChannel::new(state.music),
            vox: AtomicChannel::new(state.vox),
            master_gain: AtomicU32::new(clamp_gain(state.master.gain).to_bits()),
            master_mute: AtomicBool::new(state.master.mute),
            master_peak_l: AtomicU32::new(0),
            master_peak_r: AtomicU32::new(0),
        })
    }

    /// (gain, muted) for the master strip. Muted is a bool because the
    /// source callback short-circuits the whole frame when true.
    #[inline]
    pub fn master(&self) -> (f32, bool) {
        (load_f32(&self.master_gain), self.master_mute.load(RELAXED))
    }

    /// Observe the recent master output peaks (`|L|`, `|R|`). Called once
    /// per process cycle from the RT source callback with the maxima seen
    /// over that buffer.
    #[inline]
    pub fn observe_master_peak(&self, left: f32, right: f32) {
        if left.is_finite() && left > 0.0 {
            self.master_peak_l.fetch_max(left.to_bits(), RELAXED);
        }
        if right.is_finite() && right > 0.0 {
            self.master_peak_r.fetch_max(right.to_bits(), RELAXED);
        }
    }

    /// Read + reset every recent peak in one pass. Used by the HTTP
    /// `/mixer/levels` handler so each poll returns the peak since the
    /// previous poll.
    pub fn take_levels(&self) -> LevelsSnapshot {
        LevelsSnapshot {
            tts: self.tts.take_peak(),
            music: self.music.take_peak(),
            vox: self.vox.take_peak(),
            master_l: f32::from_bits(self.master_peak_l.swap(0, RELAXED)),
            master_r: f32::from_bits(self.master_peak_r.swap(0, RELAXED)),
        }
    }

    pub fn snapshot(&self) -> MixerState {
        MixerState {
            tts: self.tts.snapshot(),
            music: self.music.snapshot(),
            vox: self.vox.snapshot(),
            master: MasterState {
                gain: load_f32(&self.master_gain),
                mute: self.master_mute.load(RELAXED),
            },
        }
    }

    /// Apply a partial patch. Missing fields keep their current value so the
    /// client can nudge a single knob without echoing full state back.
    pub fn apply(&self, patch: MixerPatch) {
        if let Some(p) = patch.tts {
            self.tts.apply(p);
        }
        if let Some(p) = patch.music {
            self.music.apply(p);
        }
        if let Some(p) = patch.vox {
            self.vox.apply(p);
        }
        if let Some(p) = patch.master {
            if let Some(g) = p.gain {
                store_f32(&self.master_gain, clamp_gain(g));
            }
            if let Some(m) = p.mute {
                self.master_mute.store(m, RELAXED);
            }
        }
    }
}

// -----------------------------------------------------------------------------
// Wire types
// -----------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
pub struct ChannelState {
    pub gain: f32,
    pub pan: f32,
    pub mute: bool,
}

impl Default for ChannelState {
    fn default() -> Self {
        Self { gain: 1.0, pan: 0.0, mute: false }
    }
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
pub struct MasterState {
    pub gain: f32,
    pub mute: bool,
}

impl Default for MasterState {
    fn default() -> Self {
        Self { gain: 1.0, mute: false }
    }
}

#[derive(Copy, Clone, Debug, Default, Serialize, Deserialize)]
pub struct MixerState {
    #[serde(default)]
    pub tts: ChannelState,
    #[serde(default)]
    pub music: ChannelState,
    #[serde(default)]
    pub vox: ChannelState,
    #[serde(default)]
    pub master: MasterState,
}

/// Snapshot returned by `GET /mixer/levels`. Every field is a peak
/// magnitude in `[0, 1]` (or slightly beyond, since summing multiple
/// strips can transiently push a strip peak past 1.0 before the master
/// clamp — useful signal for the UI). Per-strip peaks are the max of
/// `|L|`/`|R|` for that strip's contribution to the mix bus (pre-master);
/// `master_l` / `master_r` are the post-master clamped output peaks.
#[derive(Copy, Clone, Debug, Default, Serialize)]
pub struct LevelsSnapshot {
    pub tts: f32,
    pub music: f32,
    pub vox: f32,
    pub master_l: f32,
    pub master_r: f32,
}

#[derive(Copy, Clone, Debug, Default, Deserialize)]
pub struct ChannelPatch {
    pub gain: Option<f32>,
    pub pan: Option<f32>,
    pub mute: Option<bool>,
}

#[derive(Copy, Clone, Debug, Default, Deserialize)]
pub struct MasterPatch {
    pub gain: Option<f32>,
    pub mute: Option<bool>,
}

#[derive(Copy, Clone, Debug, Default, Deserialize)]
pub struct MixerPatch {
    pub tts: Option<ChannelPatch>,
    pub music: Option<ChannelPatch>,
    pub vox: Option<ChannelPatch>,
    pub master: Option<MasterPatch>,
}

impl From<ChannelState> for ChannelPatch {
    fn from(s: ChannelState) -> Self {
        Self { gain: Some(s.gain), pan: Some(s.pan), mute: Some(s.mute) }
    }
}

impl From<MasterState> for MasterPatch {
    fn from(s: MasterState) -> Self {
        Self { gain: Some(s.gain), mute: Some(s.mute) }
    }
}

impl From<MixerState> for MixerPatch {
    fn from(s: MixerState) -> Self {
        Self {
            tts: Some(s.tts.into()),
            music: Some(s.music.into()),
            vox: Some(s.vox.into()),
            master: Some(s.master.into()),
        }
    }
}
