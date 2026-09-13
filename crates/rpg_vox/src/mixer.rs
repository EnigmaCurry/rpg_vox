//! Runtime mixer state for the pipewire source.
//!
//! Three strip families plus a master:
//!
//! * `tts` — synthesized speech from the tokio TTS runner.
//! * `music` — the `-music` companion sink.
//! * `vox`  — an operator-configured **array** of Vox channels (the count
//!   is fixed at boot from `RPG_VOX_VOX_SLOTS`, default 4). Each slot has
//!   its own display name, gain/pan/mute, and `to_output` gate so several
//!   independent capture sources (Discord user A vs. B, a couple mics on
//!   the same table, etc.) can be routed and transcribed independently.
//!
//! Values are read from the realtime source callback, so state lives in
//! atomics — `Arc<AtomicMixer>` is cheap to clone, lock-free to read from
//! the RT thread, and lock-free to update from HTTP. The vox slot count is
//! fixed for the lifetime of the process (a `Box<[VoxSlot]>` never
//! reallocates), so the callback can `for slot in &mixer.vox` without
//! taking a lock — only the *contents* of each slot mutate.
//!
//! Persisted through the existing sqlite `app_state` KV table (see
//! [`crate::store::Store::get_mixer`]/`put_mixer`). Loaded once at startup
//! and re-applied whenever the HTTP endpoint accepts a patch.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

use crate::tts::clicks::ClickPreset;

const RELAXED: Ordering = Ordering::Relaxed;

/// Upper limit for a linear gain fader (approx +6 dB). Keeping the ceiling
/// modest here means the constant-power pan curve never turns a nominal
/// unity signal into hard-clipping output on its own.
pub const MAX_GAIN: f32 = 2.0;

/// Default vox slot count. Overridable at boot via `RPG_VOX_VOX_SLOTS`
/// (see main.rs). Not a const-generic because we want runtime
/// configurability without a rebuild.
pub const DEFAULT_VOX_SLOTS: usize = 4;

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

/// One Vox slot. Combines the strip audio parameters with the metadata
/// (enable flag, display name, `to_output` gate) that used to live on the
/// AtomicMixer proper. Held inside a `Box<[VoxSlot]>` so the RT callback
/// can iterate without allocating or locking — only the atomics inside
/// each slot mutate over the process lifetime.
#[derive(Debug)]
pub struct VoxSlot {
    pub channel: AtomicChannel,
    /// Slot is "hot" for the record pipeline + mic-feed inclusion. A
    /// disabled slot still exists as a pipewire sink (so routing doesn't
    /// get lost on toggle) but its samples aren't summed into the master
    /// bus and no VAD worker is spawned for it.
    enabled: AtomicBool,
    /// Sum this slot into the master mic feed. Independent of `enabled` —
    /// the "→ MIC" toggle in the mixer maps to this. Vox slots default
    /// off so they capture-only unless the user opts one back into the
    /// output.
    to_output: AtomicBool,
    /// Display name for this slot (shown in the mixer strip label + on
    /// every transcript entry produced from this slot). Behind a mutex
    /// because the RT audio thread never reads the name — only the HTTP
    /// layer and the VAD worker do — so lock contention is a non-issue.
    name: Mutex<String>,
}

impl VoxSlot {
    fn new(state: VoxSlotState) -> Self {
        Self {
            channel: AtomicChannel::new(state.channel),
            enabled: AtomicBool::new(state.enabled),
            to_output: AtomicBool::new(state.to_output),
            name: Mutex::new(state.name),
        }
    }

    #[inline]
    pub fn enabled(&self) -> bool {
        self.enabled.load(RELAXED)
    }

    #[inline]
    pub fn to_output(&self) -> bool {
        self.to_output.load(RELAXED)
    }

    pub fn name(&self) -> String {
        self.name.lock().expect("vox slot name mutex poisoned").clone()
    }

    pub fn set_name(&self, name: String) {
        *self.name.lock().expect("vox slot name mutex poisoned") = name;
    }

    pub fn snapshot(&self) -> VoxSlotState {
        VoxSlotState {
            enabled: self.enabled(),
            to_output: self.to_output(),
            name: self.name(),
            channel: self.channel.snapshot(),
        }
    }

    fn apply(&self, patch: VoxSlotPatch) {
        if let Some(e) = patch.enabled {
            self.enabled.store(e, RELAXED);
        }
        if let Some(v) = patch.to_output {
            self.to_output.store(v, RELAXED);
        }
        if let Some(n) = patch.name {
            let n = n.trim().to_string();
            if !n.is_empty() {
                self.set_name(n);
            }
        }
        if let Some(c) = patch.channel {
            self.channel.apply(c);
        }
    }
}

/// Full mixer state. `Arc<Self>` is handed to the pipewire source thread,
/// the HTTP layer and the persistence layer — clones are cheap.
#[derive(Debug)]
pub struct AtomicMixer {
    pub tts: AtomicChannel,
    pub music: AtomicChannel,
    /// Vox slot array. Length fixed for the process lifetime — the
    /// realtime callback iterates `&*vox` without any synchronization.
    /// Access individual slots via [`Self::vox_slot`] or iterate directly.
    pub vox: Box<[VoxSlot]>,
    master_gain: AtomicU32,
    master_mute: AtomicBool,
    /// Master output peaks per side (post-master-gain, post-clip). Same
    /// fetch_max / swap-to-reset semantics as `AtomicChannel::peak`.
    master_peak_l: AtomicU32,
    master_peak_r: AtomicU32,
    /// Monotonic "stop TTS playback" counter. HTTP `POST /playback/stop`
    /// bumps this via [`Self::request_tts_stop`]; the pipewire callback
    /// notices the change vs. its last-seen value and pops-and-discards
    /// every pending frame in the TTS ring, so an in-flight clip actually
    /// falls silent instead of finishing to drain naturally. The TTS
    /// runner also reads this on entry and between push chunks so it
    /// stops loading NEW samples into the (soon-to-be-flushed) ring.
    /// Counter (rather than a bool) so races between three consumers
    /// don't lose events — anyone who cares just compares to their last
    /// snapshot.
    tts_stop_gen: AtomicU64,
    /// Monotonic "latest PlayPcm request" counter for latest-wins dedup.
    play_seq: AtomicU64,
    /// Highest `tts_stop_gen` the pw process callback has already acted on
    /// (drained the ring for). Published after each drain so the TTS
    /// runner can wait until a pending stop has been honored before it
    /// starts pushing a fresh clip.
    tts_stop_observed_gen: AtomicU64,
    /// Wait-fill click track selection. `0` = off; any other value
    /// decodes to a [`ClickPreset`] via [`ClickPreset::from_u8`].
    clicks_preset: AtomicU8,
}

impl AtomicMixer {
    pub fn new(state: MixerState) -> Arc<Self> {
        let vox: Box<[VoxSlot]> = state
            .vox
            .into_iter()
            .map(VoxSlot::new)
            .collect();
        Arc::new(Self {
            tts: AtomicChannel::new(state.tts),
            music: AtomicChannel::new(state.music),
            vox,
            master_gain: AtomicU32::new(clamp_gain(state.master.gain).to_bits()),
            master_mute: AtomicBool::new(state.master.mute),
            master_peak_l: AtomicU32::new(0),
            master_peak_r: AtomicU32::new(0),
            tts_stop_gen: AtomicU64::new(0),
            play_seq: AtomicU64::new(0),
            tts_stop_observed_gen: AtomicU64::new(0),
            clicks_preset: AtomicU8::new(0),
        })
    }

    /// Reference to a vox slot by index. Returns `None` for out-of-range
    /// indices so patch handlers can 404 instead of panicking on a
    /// client-side typo.
    #[inline]
    pub fn vox_slot(&self, index: usize) -> Option<&VoxSlot> {
        self.vox.get(index)
    }

    pub fn vox_slot_count(&self) -> usize {
        self.vox.len()
    }

    /// Select the wait-fill preset. `Some(preset)` turns the click bed on
    /// with that voicing; `None` turns it off.
    #[inline]
    pub fn set_clicks_preset(&self, preset: Option<ClickPreset>) {
        let v = preset.map(|p| p.as_u8()).unwrap_or(0);
        self.clicks_preset.store(v, RELAXED);
    }

    #[inline]
    pub fn clicks_preset(&self) -> Option<ClickPreset> {
        ClickPreset::from_u8(self.clicks_preset.load(RELAXED))
    }

    #[inline]
    pub fn request_tts_stop(&self) -> u64 {
        self.tts_stop_gen.fetch_add(1, RELAXED) + 1
    }

    #[inline]
    pub fn tts_stop_gen(&self) -> u64 {
        self.tts_stop_gen.load(RELAXED)
    }

    #[inline]
    pub fn claim_play_seq(&self) -> u64 {
        self.play_seq.fetch_add(1, RELAXED) + 1
    }

    #[inline]
    pub fn latest_play_seq(&self) -> u64 {
        self.play_seq.load(RELAXED)
    }

    #[inline]
    pub fn set_tts_stop_observed_gen(&self, gen: u64) {
        self.tts_stop_observed_gen.store(gen, RELAXED);
    }

    #[inline]
    pub fn tts_stop_observed_gen(&self) -> u64 {
        self.tts_stop_observed_gen.load(RELAXED)
    }

    #[inline]
    pub fn master(&self) -> (f32, bool) {
        (load_f32(&self.master_gain), self.master_mute.load(RELAXED))
    }

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
            vox: self.vox.iter().map(|s| s.channel.take_peak()).collect(),
            master_l: f32::from_bits(self.master_peak_l.swap(0, RELAXED)),
            master_r: f32::from_bits(self.master_peak_r.swap(0, RELAXED)),
        }
    }

    pub fn snapshot(&self) -> MixerState {
        MixerState {
            tts: self.tts.snapshot(),
            music: self.music.snapshot(),
            vox: self.vox.iter().map(|s| s.snapshot()).collect(),
            master: MasterState {
                gain: load_f32(&self.master_gain),
                mute: self.master_mute.load(RELAXED),
            },
            legacy_vox: None,
            legacy_vox_to_output: None,
        }
    }

    /// Apply a partial patch. Missing fields keep their current value so the
    /// client can nudge a single knob without echoing full state back.
    ///
    /// Vox slot patches arrive as a map keyed by stringified index — this
    /// lets the client patch several slots in one PUT (rarely useful) but
    /// keeps the common case of "one slider moved" a 1-entry map.
    pub fn apply(&self, patch: MixerPatch) {
        if let Some(p) = patch.tts {
            self.tts.apply(p);
        }
        if let Some(p) = patch.music {
            self.music.apply(p);
        }
        if let Some(slots) = patch.vox {
            for (key, slot_patch) in slots {
                if let Ok(idx) = key.parse::<usize>() {
                    if let Some(slot) = self.vox.get(idx) {
                        slot.apply(slot_patch);
                    }
                }
            }
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VoxSlotState {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub to_output: bool,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub channel: ChannelState,
}

impl VoxSlotState {
    /// Boot-time defaults for slot `index`. Slot 0 is enabled by default
    /// (the historical single Vox channel); every other slot ships
    /// disabled. Display names read "Vox 1", "Vox 2", ... consistently
    /// so a mixer with the first slot renamed looks the same shape as
    /// a mixer with the third slot renamed — the user can override any
    /// slot's name from the mixer strip.
    pub fn boot_default(index: usize) -> Self {
        Self {
            enabled: index == 0,
            to_output: false,
            name: format!("Vox {}", index + 1),
            channel: ChannelState::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MixerState {
    #[serde(default)]
    pub tts: ChannelState,
    #[serde(default)]
    pub music: ChannelState,
    /// Vox slots — as many as the process was booted with
    /// (`RPG_VOX_VOX_SLOTS`). The persisted JSON is authoritative for
    /// **length** on restart — if the operator changes the env var, the
    /// loader rescales the array in [`MixerState::resize_for`] before
    /// building the AtomicMixer.
    #[serde(default)]
    pub vox: Vec<VoxSlotState>,
    #[serde(default)]
    pub master: MasterState,
    /// Legacy field: single-vox `vox_to_output` toggle from before slots
    /// existed. Migrated into `vox[0].to_output` in [`Self::migrate`] on
    /// load. Kept as `#[serde(default)]` so old JSON parses cleanly.
    #[serde(default, skip_serializing)]
    legacy_vox_to_output: Option<bool>,
    /// Legacy single-vox channel from before slots. Migrated into
    /// `vox[0].channel` when present.
    #[serde(default, skip_serializing, rename = "legacyVox")]
    legacy_vox: Option<ChannelState>,
}

impl Default for MixerState {
    fn default() -> Self {
        Self::with_slot_count(DEFAULT_VOX_SLOTS)
    }
}

impl MixerState {
    pub fn with_slot_count(n: usize) -> Self {
        Self {
            tts: ChannelState::default(),
            music: ChannelState::default(),
            vox: (0..n).map(VoxSlotState::boot_default).collect(),
            master: MasterState::default(),
            legacy_vox_to_output: None,
            legacy_vox: None,
        }
    }

    /// Resize the vox slot list to `n` after loading. Preserves existing
    /// slot contents up to min(current, n) and appends fresh boot defaults
    /// beyond that. Shrinking drops the trailing slots.
    pub fn resize_for(&mut self, n: usize) {
        if self.vox.len() < n {
            for i in self.vox.len()..n {
                self.vox.push(VoxSlotState::boot_default(i));
            }
        } else if self.vox.len() > n {
            self.vox.truncate(n);
        }
    }

    /// One-shot migration for pre-slots JSON: fold legacy `vox` and
    /// `vox_to_output` into slot 0. Safe to call unconditionally — it's
    /// a no-op when the fields aren't set.
    pub fn migrate(&mut self) {
        if self.vox.is_empty() {
            self.vox.push(VoxSlotState::boot_default(0));
        }
        if let Some(v) = self.legacy_vox.take() {
            self.vox[0].channel = v;
        }
        if let Some(v) = self.legacy_vox_to_output.take() {
            self.vox[0].to_output = v;
        }
    }
}

/// Snapshot returned by `GET /mixer/levels`. Every field is a peak
/// magnitude in `[0, 1]` (or slightly beyond, since summing multiple
/// strips can transiently push a strip peak past 1.0 before the master
/// clamp — useful signal for the UI).
#[derive(Clone, Debug, Default, Serialize)]
pub struct LevelsSnapshot {
    pub tts: f32,
    pub music: f32,
    /// Per-vox-slot peaks, indexed by slot. Length matches the mixer's
    /// slot count.
    pub vox: Vec<f32>,
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

#[derive(Clone, Debug, Default, Deserialize)]
pub struct VoxSlotPatch {
    pub enabled: Option<bool>,
    #[serde(rename = "toOutput", alias = "to_output")]
    pub to_output: Option<bool>,
    pub name: Option<String>,
    #[serde(flatten)]
    pub channel: Option<ChannelPatch>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct MixerPatch {
    pub tts: Option<ChannelPatch>,
    pub music: Option<ChannelPatch>,
    /// Map of slot index (stringified) → per-slot patch. The wire format
    /// looks like `{ "vox": { "0": { "gain": 0.8 }, "2": { "name": "Alice" } } }`.
    /// Common case is a single-entry map (one slider dragged).
    pub vox: Option<BTreeMap<String, VoxSlotPatch>>,
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
        let vox = s
            .vox
            .into_iter()
            .enumerate()
            .map(|(i, slot)| {
                (
                    i.to_string(),
                    VoxSlotPatch {
                        enabled: Some(slot.enabled),
                        to_output: Some(slot.to_output),
                        name: Some(slot.name),
                        channel: Some(slot.channel.into()),
                    },
                )
            })
            .collect();
        Self {
            tts: Some(s.tts.into()),
            music: Some(s.music.into()),
            vox: Some(vox),
            master: Some(s.master.into()),
        }
    }
}
