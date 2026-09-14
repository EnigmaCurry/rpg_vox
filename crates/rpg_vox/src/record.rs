//! Live transcription off the `-vox` companion sink.
//!
//! One background task subscribes to the shared `vox_tap` broadcast, chunks
//! incoming stereo PCM into utterances via a coarse energy-based VAD, and
//! hands each closed utterance to the sherpa-onnx recognizer. Emitted text
//! lands in [`RecordState`] which the Record tab in the UI polls every few
//! hundred milliseconds.
//!
//! Two modes, controlled by [`RecordState`]:
//!
//! * **Not Recording** — every utterance is appended to a rolling in-memory
//!   buffer capped at [`BUFFER_MAX_BYTES`]. Oldest entries are dropped FIFO
//!   when the cap is exceeded, so the buffer never grows unbounded.
//! * **Recording** — a named "bucket" is created on `POST /record/recordings`.
//!   Every subsequent utterance is written to both the rolling buffer AND
//!   the active bucket, and every raw stereo audio chunk is also appended to
//!   the bucket so the eventual `/stop` can persist a WAV + transcript.
//!
//! When STT is disabled at startup no worker spawns and no transcripts land;
//! the record HTTP endpoints still work (the UI stays empty) so future
//! recognizer additions can drop in without further wiring.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::mixer::AtomicMixer;
use crate::stt::{StreamingSession, StreamingSttHandle, SttHandle};

/// Hard cap on the rolling Not-Recording buffer. Old entries drop FIFO when
/// the running total of `entry.text.len()` exceeds this. 50 KB ≈ a few
/// minutes of steady talking — plenty of scrollback without leaking RAM if
/// the app is left running for hours.
pub const BUFFER_MAX_BYTES: usize = 50 * 1024;

/// Longest single named recording we buffer in RAM before force-closing it.
/// 30 min at 48 kHz stereo f32 ≈ 690 MB — big but survivable, and past that
/// the operator almost certainly forgot to stop.
const RECORDING_MAX_FRAMES: usize = 48_000 * 60 * 30;

/// RMS floor for the mixed-mic gate. Below this the chunk is treated as
/// silence and may be trimmed out of the recorded mixed track (subject
/// to the hangover below). Around -46 dBFS — quiet enough that TTS
/// inter-phoneme dips still count as "sound", loud enough that dead air
/// between clips actually pauses.
const MIXED_GATE_RMS: f32 = 0.005;
/// Continue writing this many ms after the last non-silent chunk before
/// pausing the recorder. Preserves natural sentence-boundary pauses and
/// TTS phrasing gaps so playback doesn't sound stitched.
const MIXED_GATE_HANGOVER_MS: u64 = 500;

/// Energy-based VAD tuning. All decisions run at "window" granularity —
/// each window covers `VAD_WINDOW_MS` of mono audio and contributes its
/// RMS to the speaking/silent state machine. Sample-granularity checking
/// doesn't work for speech because voice signals cross zero many times
/// per cycle; instantaneous amplitude is near-zero for a large fraction of
/// samples even during loud talking.
const VAD_WINDOW_MS: u32 = 20;
/// RMS threshold in [0, 1] linear amplitude. Normal talking on a typical
/// virtual mic sits around 0.04–0.15 RMS; residual pipewire silence is
/// well under 0.001. 0.008 leaves ~15 dB of headroom above the noise
/// floor while catching soft speech.
const VAD_RMS_THRESHOLD: f32 = 0.008;
/// Consecutive voiced windows required to enter the Speaking state.
/// 150 ms filters one-off spikes (a keyboard click) without eating the
/// first syllable — the pre-roll carries audio from before this trip
/// back into the recognizer.
const VAD_SPEECH_START_MS: u32 = 150;
/// Consecutive silent windows required to close the current utterance.
/// Too short and the recognizer wakes up mid-sentence; too long and
/// there's a noticeable UI lag between "I stopped speaking" and "the
/// transcript appeared".
const VAD_SILENCE_END_MS: u32 = 700;
/// Audio kept before the Speaking trigger so the recognizer sees the
/// attack of the first word. Prevents chopping breath / lip noise that
/// carries the syllable's start.
const VAD_PRE_ROLL_MS: u32 = 300;
/// Force-close an utterance that runs past this. Guards against a hot mic
/// or steady background noise where the silent-window count never trips.
const VAD_MAX_UTTERANCE_MS: u32 = 15_000;

/// How many speaker hints to keep in the rolling ring. External senders
/// (e.g. discord_vox) typically fire one hint per "user started speaking"
/// event; on a busy Discord channel that's still only a handful per
/// second, so 256 covers well over a minute of history — plenty for the
/// finalize-time lookup window.
const HINT_RING_CAPACITY: usize = 256;
/// How far back the finalize-time hint lookup will look. Matches the
/// upper bound on typical Discord speaking-event latency; longer would
/// start pulling in stale hints from previous speakers.
const HINT_LOOKUP_WINDOW: Duration = Duration::from_secs(5);
/// How far *after* the utterance end we still accept a hint. Covers
/// the case where the transient triggered VAD before the caller's
/// speaking event fired on their side.
const HINT_LOOKUP_FUTURE_SLOP: Duration = Duration::from_millis(500);

/// One "who's talking" hint from an external source. `slot = None`
/// means the hint applies to any Vox channel (caller doesn't know
/// which one their audio lands on); `Some(i)` scopes it to that
/// specific slot so multi-source pre-mixes stay distinct.
#[derive(Debug, Clone)]
pub struct SpeakerHint {
    pub speaker: String,
    pub slot: Option<usize>,
    pub received_at: Instant,
}

/// One transcript row. `text` is whatever the recognizer produced (empty
/// utterances are dropped upstream so this is always non-empty at write
/// time — but stored `String` for later editability without a rebuild).
///
/// `channel` is the human-readable name of the audio channel this
/// utterance came from — snapshot at capture time so a later rename
/// doesn't retroactively relabel historical entries.
///
/// `audio_start_ms` + `audio_duration_ms` are only populated when the
/// utterance was captured *during a named recording*. They index into the
/// recording's audio timeline so the client can pull a WAV of the exact
/// clip that produced the transcript (useful for confirming or correcting
/// bad transcriptions).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub id: String,
    /// Unix epoch seconds. Used by the UI purely as a display timestamp.
    pub created_at: i64,
    pub text: String,
    #[serde(default)]
    pub channel: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_start_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_duration_ms: Option<u64>,
    /// External audio URL (e.g. `/widgets/<id>`) for entries whose clip
    /// isn't stored inline in the recording's own audio buffer — namely
    /// TTS entries, whose audio came out through the mic feed rather
    /// than the vox tap. When set, the client uses this as the audio
    /// `<src>` directly and ignores `audio_start_ms` / `audio_duration_ms`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_url: Option<String>,
    /// Best-guess speaker identity, resolved from the recent hint ring
    /// at utterance-finalize time (see [`RecordState::resolve_speaker`]).
    /// Populated only for vox entries when a matching hint exists; TTS
    /// entries and unmatched vox utterances leave this `None`. UI
    /// renders it inline with the channel tag so a pre-mixed channel
    /// still tells you who was talking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    /// Wall-clock unix milliseconds of when this utterance actually
    /// **started** (as opposed to `created_at`, which is the finalize /
    /// STT-decode moment). Populated for both vox (VAD-derived: end
    /// minus duration) and TTS entries. Used as the sort key when
    /// inserting into the transcript so a long clip whose STT decode
    /// finishes after a short clip that started later still appears
    /// above the short clip — matching the audible order rather than
    /// the finalize order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_wall_ms: Option<u64>,
    /// Position of this entry's start in the *mixed* recording timeline
    /// (silence-trimmed final mic feed). Distinct from `audio_start_ms`,
    /// which is the position in the per-slot recording. Computed on the
    /// fly for the active recording (via [`ActiveRecording::wall_to_mixed_ms`])
    /// and stamped permanently on entries when the recording is stopped
    /// and persisted. Used by the client's playhead animation when the
    /// mixed track is the one playing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mixed_start_ms: Option<u64>,
    /// True while this entry is a live streaming-recognition partial and
    /// the utterance hasn't been closed by VAD yet. Clients render
    /// provisional entries with a dimmed / italic style so it's obvious
    /// the text may still be revised. Cleared to `false` when the final
    /// decode replaces the entry, and defaults to `false` for entries
    /// pushed by legacy paths (offline SenseVoice finalize, TTS logs)
    /// so nothing in the persisted history is silently marked
    /// provisional after the fact.
    #[serde(default)]
    pub provisional: bool,
}

/// One contiguous stretch of non-silent audio written into the mixed
/// track. Recorded as we go so any wall-clock timestamp (e.g. a
/// transcript entry's `start_wall_ms`) can be mapped to a position in
/// the trimmed mixed timeline: within the window it's linear, in the
/// gaps between windows the mapping stays pinned to the previous
/// window's end.
#[derive(Debug, Clone, Copy)]
struct SpeechWindow {
    /// Wall-clock unix millis when this window began (the moment we
    /// left silence).
    wall_start_ms: u64,
    /// Wall-clock unix millis when this window ended (silence + hangover
    /// tripped the gate closed). `u64::MAX` while the window is still
    /// open — meaning we're still writing to the mixed track.
    wall_end_ms: u64,
    /// Position in the mixed audio timeline (ms) where this window
    /// begins. Together with `wall_start_ms` this defines a linear
    /// mapping over the window's lifetime.
    mixed_start_ms: u64,
}

/// In-flight named recording. Owned by [`RecordState`]; drained + moved to
/// the store by `POST /record/recordings/:id/stop`.
struct ActiveRecording {
    id: String,
    name: String,
    created_at: i64,
    entries: Vec<TranscriptEntry>,
    /// Interleaved stereo f32 samples at `sample_rate`. Grown as chunks
    /// arrive from every enabled slot's tap — each slot's chunk is
    /// **summed** into this shared buffer at its wall-clock offset so
    /// two channels talking simultaneously mix down together instead of
    /// concatenating (which was the earlier bug). Silence-padded when a
    /// chunk lands past the current end.
    audio: Vec<f32>,
    /// The final mic feed (mixed_tap capture): stereo interleaved f32
    /// samples at `sample_rate`, silence-gated so long dead air doesn't
    /// bloat the track. This is what the "▶ Mixed" button plays — the
    /// full rpg_vox output including TTS + music + all vox slots, as
    /// downstream sinks heard it. Mapping back to wall-clock (for the
    /// entry-highlight animation) goes through [`mixed_map`].
    mixed_audio: Vec<f32>,
    /// Piecewise wall-clock → mixed-timeline map. Non-overlapping,
    /// monotonically increasing in both axes. The last window may be
    /// "open" (wall_end_ms == u64::MAX) meaning it's the currently-
    /// active speech period.
    mixed_map: Vec<SpeechWindow>,
    /// True while we're inside a speech window (i.e. still writing to
    /// mixed_audio). Toggled by the mixed-tap worker's gate.
    mixed_open: bool,
    /// Wall-clock unix ms of the most recent non-silent chunk we saw.
    /// Compared against `MIXED_GATE_HANGOVER_MS` to decide when to
    /// close the current speech window.
    mixed_last_hot_ms: u64,
    sample_rate: u32,
    /// Wall-clock start of the recording. Used ONLY to anchor a slot's
    /// first chunk into the shared timeline — thereafter each slot
    /// advances by its own chunk length via [`slot_cursors`]. Using
    /// wall-clock for every push produced audible doubling when the
    /// tokio broadcast fired two chunks in one wake (both computed the
    /// same `elapsed_secs`, so the second one landed under the first
    /// and the `+=` mix summed the sample with itself).
    started_at: Instant,
    /// Per-slot next-write cursor in mono frames of the shared audio
    /// timeline. `None` until that slot has received its first chunk
    /// (which anchors it via wall-clock). After that, each chunk
    /// advances the cursor by exactly `chunk.len() / 2` frames so
    /// contiguous chunks from the same slot land contiguously — no
    /// self-overlap regardless of scheduling jitter. Cross-slot
    /// alignment still works because each slot's first-chunk anchor is
    /// independent.
    slot_cursors: Vec<Option<u64>>,
    /// Aborts the mixed-audio worker task on drop. `start_recording`
    /// spawns one dedicated subscriber for this recording; when the
    /// recording is finalized (or the ActiveRecording is otherwise
    /// dropped) the guard fires and the broadcast receiver goes away,
    /// dropping `monitor_tap.receiver_count()` back to zero so the RT
    /// producer skips its per-callback allocation in idle mode.
    _mixed_task: MixedWorkerGuard,
}

/// Drop-guard around the mixed-audio worker's abort handle. Aborting is
/// idempotent — a task that already finished (or was never started) is
/// unaffected — so this can live inside `ActiveRecording` without
/// worrying about which teardown path drops the outer struct.
struct MixedWorkerGuard(tokio::task::AbortHandle);

impl Drop for MixedWorkerGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl ActiveRecording {
    /// Map a wall-clock unix ms to its position in the mixed timeline.
    /// Returns `None` if the timestamp is before the recording started
    /// or the map is empty. Within a speech window the mapping is
    /// linear; in a gap it stays pinned to the window's end (i.e. the
    /// silence contributed no mixed time).
    fn wall_to_mixed_ms(&self, wall_ms: u64) -> Option<u64> {
        if self.mixed_map.is_empty() {
            return None;
        }
        // Binary search for the last window whose wall_start_ms ≤ wall_ms.
        let idx = match self
            .mixed_map
            .binary_search_by_key(&wall_ms, |w| w.wall_start_ms)
        {
            Ok(i) => i,
            Err(0) => return None, // wall_ms predates any window
            Err(i) => i - 1,
        };
        let w = &self.mixed_map[idx];
        let window_end = if w.wall_end_ms == u64::MAX {
            // Open window: cap at wall_ms itself so an in-progress entry
            // maps to its current position rather than the end.
            wall_ms
        } else {
            w.wall_end_ms
        };
        if wall_ms >= w.wall_start_ms && wall_ms <= window_end {
            Some(w.mixed_start_ms + (wall_ms - w.wall_start_ms))
        } else {
            // In the silence gap after this window — pin to window end.
            Some(w.mixed_start_ms + (window_end - w.wall_start_ms))
        }
    }
}

/// Snapshot payload for `GET /record`. Serde-serialized directly; no
/// intermediate DTO. Fields match what the Svelte page expects.
#[derive(Debug, Clone, Serialize)]
pub struct RecordStateSnapshot {
    pub mode: &'static str,
    pub buffer: Vec<TranscriptEntry>,
    pub buffer_bytes: usize,
    pub buffer_max_bytes: usize,
    /// `None` when the process is in Not Recording mode. Serialized as
    /// `null` on the wire; the UI keys off it to switch the top-of-page
    /// affordances.
    pub active_recording: Option<ActiveRecordingSnapshot>,
    /// Display names for every configured vox slot, indexed 0..N. The UI
    /// uses this as the canonical channel list.
    pub channel_names: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActiveRecordingSnapshot {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub entries: Vec<TranscriptEntry>,
    /// Approximate seconds captured so far, computed from the audio buffer
    /// length. Rough enough to drive a live "recording: 0:42" counter.
    pub duration_ms: u64,
    /// Duration (ms) of the silence-gated mixed track. Roughly what
    /// pressing "▶ Mixed" would take to play. Almost always shorter than
    /// `duration_ms` because of the silence trim.
    pub mixed_duration_ms: u64,
}

struct Inner {
    /// Rolling most-recent-first-append log. Front = oldest; back = newest.
    buffer: Vec<TranscriptEntry>,
    /// Running sum of entry byte-lengths so we don't recompute for every trim.
    buffer_bytes: usize,
    active: Option<ActiveRecording>,
}

/// Clonable handle to the shared record state. Cheap to clone — one `Arc`
/// under the hood — so the HTTP handlers and the streaming workers each
/// hold their own.
#[derive(Clone)]
pub struct RecordState {
    inner: Arc<Mutex<Inner>>,
    /// Sample rate the vox tap is producing at. Held here so
    /// `start_recording` can stamp the bucket with the right rate without
    /// threading it through every call site.
    sample_rate: u32,
    /// Shared mixer atomics — the authoritative source for every Vox
    /// slot's display name. Snapshots go onto each transcript entry at
    /// capture time so historical entries keep their at-capture label
    /// even after a subsequent rename.
    mixer: Arc<AtomicMixer>,
    /// Bounded ring of recent speaker hints from external sources (e.g.
    /// discord_vox POSTing to /record/hint). Consulted at every VAD
    /// finalize to attribute utterances on a shared / pre-mixed vox
    /// channel to the person actually talking. Kept behind its own
    /// mutex — hints arrive on the HTTP thread pool, VAD finalizes on
    /// tokio worker tasks; both paths are short so contention is
    /// negligible.
    hints: Arc<Mutex<VecDeque<SpeakerHint>>>,
    /// Fan-out for the OBS subtitles overlay: finalized vox transcript
    /// entries (only while a recording is active) plus explicit
    /// recording-active state transitions so the overlay can swap
    /// between the caption view and the "suspended" placeholder as
    /// soon as recording is started or stopped. Send failures (no
    /// subscribers) are ignored — this is a fire-and-forget channel.
    subtitles: broadcast::Sender<SubtitleEvent>,
    /// Post-mix mic feed. `start_recording` subscribes here to capture
    /// the silence-gated mixed track for the in-flight recording; the
    /// subscription is dropped when the recording finalizes so the RT
    /// producer sees `receiver_count == 0` in idle mode and skips the
    /// per-callback heap allocation that feeds this tap. Held on
    /// `RecordState` so per-recording worker spawns can pick it up
    /// without extra plumbing through the HTTP handlers.
    monitor_tap: broadcast::Sender<Arc<[f32]>>,
}

/// Events fanned out to OBS subtitle overlay subscribers.
///
/// * `Entry` — upsert-by-id semantics. Fires both for streaming
///   partials (with `provisional=true`) and for the polished final
///   that replaces them (`provisional=false`). A client keyed by
///   `entry.id` renders each event as either "new" or "update text
///   for existing".
/// * `EntryRemove` — pull an already-broadcast entry off the overlay,
///   used when a streaming partial's final decode matched the
///   false-positive filter and we don't want the stale partial to
///   linger.
/// * `RecordingActive` — swap between the caption view and the
///   "suspended" placeholder without waiting for a poll.
#[derive(Debug, Clone)]
pub enum SubtitleEvent {
    Entry(TranscriptEntry),
    EntryRemove(String),
    RecordingActive(bool),
}

impl RecordState {
    pub fn new(
        sample_rate: u32,
        mixer: Arc<AtomicMixer>,
        monitor_tap: broadcast::Sender<Arc<[f32]>>,
    ) -> Self {
        let (subtitles, _) = broadcast::channel::<SubtitleEvent>(32);
        Self {
            inner: Arc::new(Mutex::new(Inner {
                buffer: Vec::new(),
                buffer_bytes: 0,
                active: None,
            })),
            sample_rate,
            mixer,
            hints: Arc::new(Mutex::new(VecDeque::with_capacity(HINT_RING_CAPACITY))),
            subtitles,
            monitor_tap,
        }
    }

    /// Subscribe to the OBS subtitles fan-out. Yields finalized vox
    /// entries (only while a named recording is in flight) plus
    /// recording-active state transitions. Broadcast lag drops the
    /// oldest queued event; OBS overlays only care about the newest
    /// text and the current state, both of which the caller can
    /// re-derive on lag via [`Self::is_recording`].
    pub fn subscribe_subtitles(&self) -> broadcast::Receiver<SubtitleEvent> {
        self.subtitles.subscribe()
    }

    /// Record a "who's speaking" hint. `slot = None` means the hint
    /// applies to any Vox channel; `Some(i)` scopes it to that slot.
    /// Empty speaker strings are ignored so a fumbled call can't
    /// pollute the ring with blank identities.
    pub fn push_hint(&self, speaker: String, slot: Option<usize>) {
        let speaker = speaker.trim().to_string();
        if speaker.is_empty() {
            return;
        }
        let hint = SpeakerHint {
            speaker,
            slot,
            received_at: Instant::now(),
        };
        let mut g = self.hints.lock().expect("hint ring mutex poisoned");
        if g.len() >= HINT_RING_CAPACITY {
            g.pop_front();
        }
        g.push_back(hint);
    }

    /// Look up the most likely speaker for a vox utterance closing on
    /// `slot` at approximately `when`. Walks the ring newest-first,
    /// picks the first hint whose scope matches the slot (or is scope-
    /// less) and whose `received_at` is inside the lookup window.
    /// Returns `None` when no hint is applicable — the transcript entry
    /// then just carries its channel tag without a speaker label.
    pub fn resolve_speaker(&self, slot: usize, when: Instant) -> Option<String> {
        let g = self.hints.lock().expect("hint ring mutex poisoned");
        let earliest = when.checked_sub(HINT_LOOKUP_WINDOW)?;
        let latest = when.checked_add(HINT_LOOKUP_FUTURE_SLOP)?;
        // Iterate newest-first — the caller wants "who was talking at
        // the end of this clip", so the most recent hint that fits the
        // window wins even if an older one is also in range.
        for hint in g.iter().rev() {
            if hint.received_at < earliest || hint.received_at > latest {
                continue;
            }
            if let Some(s) = hint.slot {
                if s != slot {
                    continue;
                }
            }
            return Some(hint.speaker.clone());
        }
        None
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Display name for a specific vox slot. Falls back to
    /// `"Vox <slot+1>"` for out-of-range indices so transcript entries
    /// always carry a label consistent with the mixer's boot defaults.
    pub fn channel_name(&self, slot: usize) -> String {
        self.mixer
            .vox_slot(slot)
            .map(|s| s.name())
            .unwrap_or_else(|| format!("Vox {}", slot + 1))
    }

    pub fn snapshot(&self) -> RecordStateSnapshot {
        let g = self.inner.lock().expect("record state mutex poisoned");
        let mode = if g.active.is_some() { "recording" } else { "idle" };
        let active_recording = g.active.as_ref().map(|a| {
            // Project each entry with a freshly-computed `mixed_start_ms`
            // so the client's mixed-audio playhead can highlight the
            // right entry even mid-recording, before any of this is
            // stamped into the persisted transcript at stop time.
            let entries: Vec<TranscriptEntry> = a
                .entries
                .iter()
                .map(|e| {
                    let mut out = e.clone();
                    if let Some(wall) = e.start_wall_ms {
                        out.mixed_start_ms = a.wall_to_mixed_ms(wall);
                    }
                    out
                })
                .collect();
            ActiveRecordingSnapshot {
                id: a.id.clone(),
                name: a.name.clone(),
                created_at: a.created_at,
                entries,
                duration_ms: audio_duration_ms(a.audio.len(), a.sample_rate),
                mixed_duration_ms: audio_duration_ms(a.mixed_audio.len(), a.sample_rate),
            }
        });
        // Channel names for every configured slot — the UI uses these
        // as the master list of channels, keying its per-channel rendering.
        let channel_names: Vec<String> = (0..self.mixer.vox_slot_count())
            .map(|i| self.channel_name(i))
            .collect();
        RecordStateSnapshot {
            mode,
            buffer: g.buffer.clone(),
            buffer_bytes: g.buffer_bytes,
            buffer_max_bytes: BUFFER_MAX_BYTES,
            active_recording,
            channel_names,
        }
    }

    /// Snapshot the currently-active recording's full audio as stereo
    /// pairs, plus its sample rate. Returns `None` when nothing is
    /// active or the id doesn't match — the caller falls back to disk.
    /// Used by `/record/recordings/:id/audio` to serve the in-flight
    /// mix before the recording has been stopped and persisted.
    pub fn active_audio(&self, id: &str) -> Option<(Vec<[f32; 2]>, u32)> {
        let g = self.inner.lock().expect("record state mutex poisoned");
        let active = g.active.as_ref()?;
        if active.id != id {
            return None;
        }
        let sr = active.sample_rate;
        let pairs: Vec<[f32; 2]> = active
            .audio
            .chunks_exact(2)
            .map(|c| [c[0], c[1]])
            .collect();
        Some((pairs, sr))
    }

    /// Snapshot the currently-active recording's silence-gated mixed
    /// track as stereo pairs. Same conventions as [`Self::active_audio`]:
    /// returns `None` when nothing is active or the id doesn't match.
    pub fn active_mixed_audio(&self, id: &str) -> Option<(Vec<[f32; 2]>, u32)> {
        let g = self.inner.lock().expect("record state mutex poisoned");
        let active = g.active.as_ref()?;
        if active.id != id {
            return None;
        }
        let sr = active.sample_rate;
        let pairs: Vec<[f32; 2]> = active
            .mixed_audio
            .chunks_exact(2)
            .map(|c| [c[0], c[1]])
            .collect();
        Some((pairs, sr))
    }

    /// Extract a range of the currently-active recording's audio (RAM),
    /// or `None` when the id doesn't match or the range is empty / OOB.
    /// Used by the segment endpoint to serve utterance clips before the
    /// recording is stopped + persisted.
    pub fn active_audio_segment(
        &self,
        id: &str,
        start_ms: u64,
        duration_ms: u64,
    ) -> Option<(Vec<[f32; 2]>, u32)> {
        let g = self.inner.lock().expect("record state mutex poisoned");
        let active = g.active.as_ref()?;
        if active.id != id {
            return None;
        }
        let sr = active.sample_rate;
        let start_frame = (start_ms * sr as u64 / 1000) as usize;
        let n_frames = (duration_ms * sr as u64 / 1000) as usize;
        let end_frame = start_frame.saturating_add(n_frames);
        let total_frames = active.audio.len() / 2;
        if start_frame >= total_frames || n_frames == 0 {
            return None;
        }
        let end_frame = end_frame.min(total_frames);
        let mut pairs = Vec::with_capacity(end_frame - start_frame);
        for f in start_frame..end_frame {
            let base = f * 2;
            pairs.push([active.audio[base], active.audio[base + 1]]);
        }
        Some((pairs, sr))
    }

    /// Begin a named recording. Fails (returns `Err`) if one is already in
    /// flight — the UI should only surface a "New recording" affordance in
    /// idle mode, but a stale click on a fast-double-tap could race here.
    pub fn start_recording(&self, name: String) -> Result<String, &'static str> {
        let name = name.trim().to_string();
        let name = if name.is_empty() {
            format!("Recording {}", chrono::Utc::now().format("%Y-%m-%d %H:%M:%S"))
        } else {
            name
        };
        let mut g = self.inner.lock().expect("record state mutex poisoned");
        if g.active.is_some() {
            return Err("a recording is already in progress");
        }
        let id = Uuid::new_v4().to_string();
        let slot_count = self.mixer.vox_slot_count();
        // Subscribe to the monitor tap ONLY for the lifetime of this
        // recording. Keeping a subscriber alive between recordings
        // forces the RT source callback into its "someone's listening"
        // branch which allocates `Vec::with_capacity` + `Arc::from` per
        // pipewire cycle — enough at a 256-frame quantum to accumulate
        // tens of xruns/sec (audible as periodic clicks in any local
        // monitor sink).
        let mut rx = self.monitor_tap.subscribe();
        let worker_state = self.clone();
        let recording_id = id.clone();
        let handle = tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(chunk) => {
                        worker_state.push_mixed_audio(&chunk);
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(
                            recording = %recording_id,
                            missed = n,
                            "record mixed worker lagged on monitor tap",
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        info!(
                            recording = %recording_id,
                            "monitor tap closed; record mixed worker exiting",
                        );
                        return;
                    }
                }
            }
        });
        g.active = Some(ActiveRecording {
            id: id.clone(),
            name,
            created_at: unix_now(),
            entries: Vec::new(),
            audio: Vec::new(),
            mixed_audio: Vec::new(),
            mixed_map: Vec::new(),
            mixed_open: false,
            mixed_last_hot_ms: 0,
            sample_rate: self.sample_rate,
            started_at: Instant::now(),
            slot_cursors: vec![None; slot_count],
            _mixed_task: MixedWorkerGuard(handle.abort_handle()),
        });
        drop(g);
        let _ = self.subtitles.send(SubtitleEvent::RecordingActive(true));
        Ok(id)
    }

    /// Take the in-flight recording out of state. Caller (the /stop handler)
    /// then encodes both WAVs and hands the transcript JSON to the store.
    /// Returns `None` when nothing is active OR when the id doesn't match
    /// the current bucket — race-safe against a double-stop.
    ///
    /// Before draining, close any still-open speech window in the mixed
    /// map and stamp each entry with its final `mixed_start_ms` so the
    /// persisted transcript can drive the mixed-audio playhead without
    /// needing to re-derive the map at read time.
    pub fn take_active(&self, id: &str) -> Option<TakenRecording> {
        let mut g = self.inner.lock().expect("record state mutex poisoned");
        let active_id_matches = g.active.as_ref().map(|a| a.id == id).unwrap_or(false);
        if !active_id_matches {
            return None;
        }
        let mut active = g.active.take()?;
        // Emit the state transition immediately so any OBS overlay
        // subscribers flip to the suspended placeholder without waiting
        // for the encode / persist pipeline below to finish.
        let _ = self.subtitles.send(SubtitleEvent::RecordingActive(false));
        // Close any still-open speech window at "now" so wall_to_mixed_ms
        // yields a definite value rather than pinning to Instant-based
        // extrapolation. Without this, the last entry recorded right
        // before stop would land at wall_ms > wall_end_ms and get pinned
        // to the window's end instead of its real position.
        if let Some(last) = active.mixed_map.last_mut() {
            if last.wall_end_ms == u64::MAX {
                last.wall_end_ms = unix_now_ms();
            }
        }
        // Collect the mapped values first so the &active borrow releases
        // before the &mut active.entries iteration below (the borrow
        // checker isn't smart enough to see that wall_to_mixed_ms only
        // reads mixed_map, not entries).
        let mapped: Vec<Option<u64>> = active
            .entries
            .iter()
            .map(|e| e.start_wall_ms.and_then(|w| active.wall_to_mixed_ms(w)))
            .collect();
        for (entry, mixed) in active.entries.iter_mut().zip(mapped) {
            entry.mixed_start_ms = mixed;
        }
        Some(TakenRecording {
            id: active.id,
            name: active.name,
            created_at: active.created_at,
            entries: active.entries,
            audio: active.audio,
            mixed_audio: active.mixed_audio,
            sample_rate: active.sample_rate,
        })
    }

    /// Empty the rolling ephemeral buffer. Does NOT touch the in-flight
    /// active recording — clearing what the user is looking at on the
    /// Live pane must never wipe a session that's still capturing.
    pub fn clear_buffer(&self) {
        let mut g = self.inner.lock().expect("record state mutex poisoned");
        g.buffer.clear();
        g.buffer_bytes = 0;
    }

    /// Update the text on a transcript entry that lives in either the
    /// rolling ephemeral buffer or the currently-active recording.
    /// Returns `true` when a matching entry was found and mutated. Empty
    /// / whitespace-only text is rejected — the caller preserves the
    /// original rather than silently blanking a row.
    ///
    /// The `text` field is the only thing the "correct my transcription"
    /// flow needs; length rebalancing on the rolling buffer's byte total
    /// keeps its cap semantics intact after an edit that grows or
    /// shrinks the entry.
    pub fn update_entry_text(&self, entry_id: &str, text: String) -> bool {
        let text = text.trim().to_string();
        if text.is_empty() {
            return false;
        }
        let mut g = self.inner.lock().expect("record state mutex poisoned");
        // Buffer edit: mutate text in place, adjust byte total by the
        // delta. Splitting the read of the new length from the mutable
        // borrow keeps the borrow checker happy without an extra clone.
        let mut buffer_hit = false;
        let mut delta: (usize, usize) = (0, 0); // (old_len, new_len)
        for entry in g.buffer.iter_mut() {
            if entry.id == entry_id {
                delta = (entry.text.len(), text.len());
                entry.text = text.clone();
                buffer_hit = true;
                break;
            }
        }
        if buffer_hit {
            g.buffer_bytes = g
                .buffer_bytes
                .saturating_sub(delta.0)
                .saturating_add(delta.1);
        }
        // Mirror buffer→active so an entry that lives in BOTH views
        // (a VAD utterance captured mid-record) stays consistent.
        if let Some(active) = g.active.as_mut() {
            for entry in active.entries.iter_mut() {
                if entry.id == entry_id {
                    entry.text = text.clone();
                    return true;
                }
            }
        }
        buffer_hit
    }

    /// Rename the in-flight recording if `id` matches the active one.
    /// Returns `true` when the rename landed, `false` when nothing was
    /// active or the id didn't match — the HTTP handler then falls
    /// through to the persisted-recording rename path.
    pub fn rename_active(&self, id: &str, name: String) -> bool {
        let name = name.trim().to_string();
        if name.is_empty() {
            return false;
        }
        let mut g = self.inner.lock().expect("record state mutex poisoned");
        let Some(active) = g.active.as_mut() else {
            return false;
        };
        if active.id != id {
            return false;
        }
        active.name = name;
        true
    }

    /// Discard any in-flight recording without persisting. Used by
    /// `DELETE /record/recordings/:id` when the user cancels mid-record.
    /// Idempotent — returns `false` if nothing was active OR the id didn't
    /// match.
    pub fn cancel_active(&self, id: &str) -> bool {
        let mut g = self.inner.lock().expect("record state mutex poisoned");
        let matches = g.active.as_ref().map(|a| a.id == id).unwrap_or(false);
        if !matches {
            return false;
        }
        g.active = None;
        drop(g);
        let _ = self.subtitles.send(SubtitleEvent::RecordingActive(false));
        true
    }

    /// Append a completed utterance. Fires from the streaming worker after
    /// each VAD-closed segment. Always pushes to the rolling buffer; also
    /// pushes to the active bucket when one exists (with the audio-range
    /// metadata so the client can replay the clip that produced the text).
    ///
    /// Skips entries with no alphanumeric content — SenseVoice sometimes
    /// resolves a marginal segment to just "." or "。" which is meaningless
    /// as a transcript and would pollute the UI + logs. Also drops known
    /// single-word false-positive transcripts (see [`is_false_positive`]).
    fn push_transcript(
        &self,
        slot: usize,
        text: String,
        audio_start_ms: Option<u64>,
        audio_duration_ms: Option<u64>,
        speaker: Option<String>,
        start_wall_ms: Option<u64>,
    ) {
        if !text.chars().any(|c| c.is_alphanumeric()) {
            return;
        }
        if is_false_positive(&text) {
            return;
        }
        let channel = self.channel_name(slot);
        let entry = TranscriptEntry {
            id: Uuid::new_v4().to_string(),
            created_at: unix_now(),
            text,
            channel,
            audio_start_ms,
            audio_duration_ms,
            audio_url: None,
            speaker,
            start_wall_ms,
            // Filled in per-snapshot for the active recording (see
            // snapshot()) and stamped permanently at stop time.
            mixed_start_ms: None,
            provisional: false,
        };
        let mut g = self.inner.lock().expect("record state mutex poisoned");
        // Rolling buffer: insert at the position that keeps entries sorted
        // by wall-clock start time (see `insert_ordered`). Two slots'
        // utterances that finalized out-of-order relative to when they
        // started still land in the right sequence in the log. Trim from
        // the front — position 0 is the oldest by construction.
        g.buffer_bytes += entry.text.len();
        insert_ordered(&mut g.buffer, entry.clone());
        while g.buffer_bytes > BUFFER_MAX_BYTES && g.buffer.len() > 1 {
            let dropped = g.buffer.remove(0);
            g.buffer_bytes = g.buffer_bytes.saturating_sub(dropped.text.len());
        }
        let recording = g.active.is_some();
        if let Some(active) = g.active.as_mut() {
            insert_ordered(&mut active.entries, entry.clone());
        }
        drop(g);
        if recording {
            let _ = self.subtitles.send(SubtitleEvent::Entry(entry));
        }
    }

    /// Insert or update a streaming-partial transcript entry keyed by
    /// `entry_id`. If an entry with that id already exists (in either
    /// the rolling buffer or the active recording's entries), its text
    /// is replaced in place; otherwise a new entry is created.
    /// Broadcasts a [`SubtitleEvent::Entry`] with `provisional = true`
    /// so subscribers can render mid-utterance updates.
    ///
    /// No false-positive filter here — partials should show the live
    /// hypothesis even if it briefly matches a filter phrase; the
    /// filter runs only at [`Self::finalize_transcript`] time when the
    /// final text is what we're deciding about.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn upsert_partial(
        &self,
        entry_id: &str,
        slot: usize,
        text: String,
        audio_start_ms: Option<u64>,
        audio_duration_ms: Option<u64>,
        speaker: Option<String>,
        start_wall_ms: Option<u64>,
    ) {
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        let channel = self.channel_name(slot);
        let mut g = self.inner.lock().expect("record state mutex poisoned");
        // Update-in-place if the id is already present. Adjust the
        // rolling-buffer byte counter by the length delta so the cap
        // math stays correct across a partial's text growth.
        let mut updated_entry: Option<TranscriptEntry> = None;
        let mut buffer_delta: Option<(usize, usize)> = None;
        for entry in g.buffer.iter_mut() {
            if entry.id == entry_id {
                let old_len = entry.text.len();
                entry.text = trimmed.clone();
                entry.channel = channel.clone();
                entry.audio_start_ms = audio_start_ms;
                entry.audio_duration_ms = audio_duration_ms;
                entry.speaker = speaker.clone();
                entry.start_wall_ms = start_wall_ms;
                entry.provisional = true;
                buffer_delta = Some((old_len, entry.text.len()));
                updated_entry = Some(entry.clone());
                break;
            }
        }
        if let Some((old, new)) = buffer_delta {
            if new >= old {
                g.buffer_bytes += new - old;
            } else {
                g.buffer_bytes = g.buffer_bytes.saturating_sub(old - new);
            }
        }
        // Mirror the update into the active recording if present.
        if let Some(active) = g.active.as_mut() {
            for entry in active.entries.iter_mut() {
                if entry.id == entry_id {
                    entry.text = trimmed.clone();
                    entry.channel = channel.clone();
                    entry.audio_start_ms = audio_start_ms;
                    entry.audio_duration_ms = audio_duration_ms;
                    entry.speaker = speaker.clone();
                    entry.start_wall_ms = start_wall_ms;
                    entry.provisional = true;
                    if updated_entry.is_none() {
                        updated_entry = Some(entry.clone());
                    }
                    break;
                }
            }
        }
        let entry = updated_entry.unwrap_or_else(|| {
            let entry = TranscriptEntry {
                id: entry_id.to_string(),
                created_at: unix_now(),
                text: trimmed,
                channel,
                audio_start_ms,
                audio_duration_ms,
                audio_url: None,
                speaker,
                start_wall_ms,
                mixed_start_ms: None,
                provisional: true,
            };
            g.buffer_bytes += entry.text.len();
            insert_ordered(&mut g.buffer, entry.clone());
            while g.buffer_bytes > BUFFER_MAX_BYTES && g.buffer.len() > 1 {
                let dropped = g.buffer.remove(0);
                g.buffer_bytes = g.buffer_bytes.saturating_sub(dropped.text.len());
            }
            if let Some(active) = g.active.as_mut() {
                insert_ordered(&mut active.entries, entry.clone());
            }
            entry
        });
        let recording = g.active.is_some();
        drop(g);
        if recording {
            let _ = self.subtitles.send(SubtitleEvent::Entry(entry));
        }
    }

    /// Close out a streaming partial with its polished final text.
    /// Runs the same false-positive filter that `push_transcript`
    /// applies: if the final text matches, the partial is *removed*
    /// (from buffer, active, and via a `SubtitleEvent::EntryRemove`)
    /// rather than promoted. Otherwise the entry is updated in place
    /// with `provisional = false` and rebroadcast as a final.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn finalize_partial(
        &self,
        entry_id: &str,
        slot: usize,
        text: String,
        audio_start_ms: Option<u64>,
        audio_duration_ms: Option<u64>,
        speaker: Option<String>,
        start_wall_ms: Option<u64>,
    ) {
        let trimmed = text.trim().to_string();
        let is_junk = trimmed.is_empty()
            || !trimmed.chars().any(|c| c.is_alphanumeric())
            || is_false_positive(&trimmed);
        if is_junk {
            // Remove any existing partial rather than leaving stale text.
            self.remove_entry(entry_id);
            return;
        }
        let channel = self.channel_name(slot);
        let mut g = self.inner.lock().expect("record state mutex poisoned");
        let mut finalized: Option<TranscriptEntry> = None;
        let mut buffer_delta: Option<(usize, usize)> = None;
        for entry in g.buffer.iter_mut() {
            if entry.id == entry_id {
                let old_len = entry.text.len();
                entry.text = trimmed.clone();
                entry.channel = channel.clone();
                entry.audio_start_ms = audio_start_ms;
                entry.audio_duration_ms = audio_duration_ms;
                entry.speaker = speaker.clone();
                entry.start_wall_ms = start_wall_ms;
                entry.provisional = false;
                buffer_delta = Some((old_len, entry.text.len()));
                finalized = Some(entry.clone());
                break;
            }
        }
        if let Some((old, new)) = buffer_delta {
            if new >= old {
                g.buffer_bytes += new - old;
            } else {
                g.buffer_bytes = g.buffer_bytes.saturating_sub(old - new);
            }
        }
        if let Some(active) = g.active.as_mut() {
            for entry in active.entries.iter_mut() {
                if entry.id == entry_id {
                    entry.text = trimmed.clone();
                    entry.channel = channel.clone();
                    entry.audio_start_ms = audio_start_ms;
                    entry.audio_duration_ms = audio_duration_ms;
                    entry.speaker = speaker.clone();
                    entry.start_wall_ms = start_wall_ms;
                    entry.provisional = false;
                    if finalized.is_none() {
                        finalized = Some(entry.clone());
                    }
                    break;
                }
            }
        }
        // No partial ever landed (fast utterance with no partial
        // emitted yet, or the entry aged out of the rolling cap
        // between partial and final). Insert as a fresh final so the
        // transcript isn't missing an utterance.
        let entry = finalized.unwrap_or_else(|| {
            let entry = TranscriptEntry {
                id: entry_id.to_string(),
                created_at: unix_now(),
                text: trimmed,
                channel,
                audio_start_ms,
                audio_duration_ms,
                audio_url: None,
                speaker,
                start_wall_ms,
                mixed_start_ms: None,
                provisional: false,
            };
            g.buffer_bytes += entry.text.len();
            insert_ordered(&mut g.buffer, entry.clone());
            while g.buffer_bytes > BUFFER_MAX_BYTES && g.buffer.len() > 1 {
                let dropped = g.buffer.remove(0);
                g.buffer_bytes = g.buffer_bytes.saturating_sub(dropped.text.len());
            }
            if let Some(active) = g.active.as_mut() {
                insert_ordered(&mut active.entries, entry.clone());
            }
            entry
        });
        let recording = g.active.is_some();
        drop(g);
        if recording {
            let _ = self.subtitles.send(SubtitleEvent::Entry(entry));
        }
    }

    /// Delete a transcript entry by id from both the rolling buffer
    /// and the active recording, and broadcast a
    /// [`SubtitleEvent::EntryRemove`] so overlay subscribers drop it
    /// from their view. Used when a streaming partial's final text
    /// turned out to be a false-positive filter match.
    pub fn remove_entry(&self, entry_id: &str) {
        let mut g = self.inner.lock().expect("record state mutex poisoned");
        let mut found = false;
        if let Some(pos) = g.buffer.iter().position(|e| e.id == entry_id) {
            let dropped = g.buffer.remove(pos);
            g.buffer_bytes = g.buffer_bytes.saturating_sub(dropped.text.len());
            found = true;
        }
        if let Some(active) = g.active.as_mut() {
            if let Some(pos) = active.entries.iter().position(|e| e.id == entry_id) {
                active.entries.remove(pos);
                found = true;
            }
        }
        let recording = g.active.is_some();
        drop(g);
        if found && recording {
            let _ = self
                .subtitles
                .send(SubtitleEvent::EntryRemove(entry_id.to_string()));
        }
    }

    /// Log a TTS playback into the rolling ephemeral buffer, and into
    /// the active recording when one is in flight. Text is the utterance;
    /// `audio_url` (e.g. `/widgets/<id>`) points at the cached clip so
    /// playback survives the recording even though TTS audio doesn't
    /// flow through the vox tap. `voice_label` is a human-readable tag
    /// for the voice/profile that spoke — rendered inline with the TTS
    /// channel tag so the log tells you *which* voice it was.
    ///
    /// Always appends to the buffer so the Live view shows every TTS
    /// clip the user hears — including clips played with no recording
    /// active. Buffer trimming follows the same FIFO cap as vox entries.
    pub fn push_tts(&self, text: String, audio_url: Option<String>, voice_label: Option<String>) {
        let entry = TranscriptEntry {
            id: Uuid::new_v4().to_string(),
            created_at: unix_now(),
            text,
            channel: "TTS".to_string(),
            audio_start_ms: None,
            audio_duration_ms: None,
            audio_url,
            // Reuses the same `speaker` field vox entries use for hint
            // attribution — the Record UI already renders it inline with
            // the channel tag, so no extra field is needed.
            speaker: voice_label.and_then(|s| {
                let t = s.trim().to_string();
                (!t.is_empty()).then_some(t)
            }),
            // TTS is logged the moment the play call fires, so "now" is
            // the utterance's start on the same wall-clock the vox VAD
            // stamps into its entries — insert_ordered slots this in
            // beside vox entries at the right chronological spot.
            start_wall_ms: Some(unix_now_ms()),
            mixed_start_ms: None,
            provisional: false,
        };
        let mut g = self.inner.lock().expect("record state mutex poisoned");
        g.buffer_bytes += entry.text.len();
        insert_ordered(&mut g.buffer, entry.clone());
        while g.buffer_bytes > BUFFER_MAX_BYTES && g.buffer.len() > 1 {
            let dropped = g.buffer.remove(0);
            g.buffer_bytes = g.buffer_bytes.saturating_sub(dropped.text.len());
        }
        if let Some(active) = g.active.as_mut() {
            insert_ordered(&mut active.entries, entry);
        }
    }

    /// Whether a recording is currently in flight. Cheap — one mutex
    /// lock, no allocation. Used by the streaming STT gate and the OBS
    /// subtitles subscription to key off the active/idle transition.
    pub fn is_recording(&self) -> bool {
        self.inner
            .lock()
            .expect("record state mutex poisoned")
            .active
            .is_some()
    }

    /// Sum a stereo chunk into the in-flight recording at its
    /// wall-clock offset. Called on every incoming vox_tap chunk from
    /// every enabled slot — chunks from different slots that arrive at
    /// the same wall-clock moment sum together into the same audio
    /// positions, preserving overlapping voices.
    ///
    /// Silence-pads when the chunk lands past the current buffer end
    /// (both when a fresh recording is filling in samples for the first
    /// time and when a slot goes quiet for a while then resumes).
    /// Returns the offset (in frames) where the chunk was mixed so the
    /// caller can stamp utterance start positions against the same
    /// timeline.
    /// Append a stereo chunk to the mixed track if it clears the
    /// silence gate. Returns `true` if the chunk actually landed (either
    /// because it was hot, or because we're inside the hangover window
    /// after recent audio). No-op when nothing is being recorded.
    ///
    /// Silence-gate behavior:
    ///   * chunk RMS > MIXED_GATE_RMS → write, open speech window if
    ///     one isn't already open, refresh `mixed_last_hot_ms`.
    ///   * chunk RMS ≤ threshold but within MIXED_GATE_HANGOVER_MS of
    ///     the last hot chunk → still write (natural pause preservation).
    ///   * chunk RMS ≤ threshold past the hangover → close the current
    ///     window (if one is open) and skip.
    fn push_mixed_audio(&self, chunk: &[f32]) -> bool {
        let mut g = self.inner.lock().expect("record state mutex poisoned");
        let Some(active) = g.active.as_mut() else {
            return false;
        };
        if chunk.is_empty() {
            return false;
        }
        let sum_sq: f32 = chunk.iter().map(|s| s * s).sum();
        let rms = (sum_sq / chunk.len() as f32).sqrt();
        let hot = rms > MIXED_GATE_RMS;
        let now_ms = unix_now_ms();

        if hot {
            active.mixed_last_hot_ms = now_ms;
            if !active.mixed_open {
                let mixed_start_ms = audio_duration_ms(active.mixed_audio.len(), active.sample_rate);
                active.mixed_map.push(SpeechWindow {
                    wall_start_ms: now_ms,
                    wall_end_ms: u64::MAX,
                    mixed_start_ms,
                });
                active.mixed_open = true;
            }
        } else if active.mixed_open {
            let since_hot = now_ms.saturating_sub(active.mixed_last_hot_ms);
            if since_hot > MIXED_GATE_HANGOVER_MS {
                // Close the window. wall_end_ms is the last hot moment
                // *plus* the hangover so what got written matches what
                // the map claims — the hangover chunks were still
                // appended.
                if let Some(last) = active.mixed_map.last_mut() {
                    last.wall_end_ms =
                        active.mixed_last_hot_ms.saturating_add(MIXED_GATE_HANGOVER_MS);
                }
                active.mixed_open = false;
                return false;
            }
        } else {
            // Silent and no window open — pure silence, nothing to do.
            return false;
        }

        // Cap so a stuck recorder can't consume unbounded RAM. Same
        // horizon as the per-slot audio buffer.
        let max_samples = RECORDING_MAX_FRAMES * 2;
        let remaining = max_samples.saturating_sub(active.mixed_audio.len());
        if remaining == 0 {
            return false;
        }
        let take = remaining.min(chunk.len());
        active.mixed_audio.extend_from_slice(&chunk[..take]);
        true
    }

    fn push_audio(&self, slot: usize, chunk: &[f32]) -> Option<u64> {
        let mut g = self.inner.lock().expect("record state mutex poisoned");
        let active = g.active.as_mut()?;
        let max_samples = RECORDING_MAX_FRAMES * 2;
        let sample_rate = active.sample_rate;
        // stereo interleaved f32 → chunk_frames = pairs written per push.
        let chunk_frames = (chunk.len() / 2) as u64;
        if chunk_frames == 0 {
            return None;
        }
        // Grow `slot_cursors` on demand — the mixer's slot count is
        // fixed for the process lifetime but `push_audio` should be
        // resilient to a caller who passes an out-of-range slot.
        if slot >= active.slot_cursors.len() {
            active.slot_cursors.resize(slot + 1, None);
        }
        // Anchor on first chunk: wall-clock position where this slot
        // begins contributing. Later chunks ignore wall-clock and
        // advance strictly by their length, so tokio scheduling
        // jitter can't cause two consecutive chunks to overlap and
        // double-sum with themselves.
        let target_frame = match active.slot_cursors[slot] {
            Some(cur) => cur,
            None => {
                let elapsed_secs = active.started_at.elapsed().as_secs_f64();
                (elapsed_secs * sample_rate as f64).max(0.0).round() as u64
            }
        };
        let target_offset = ((target_frame as usize) * 2).min(max_samples);
        // Silence-fill up to the target if the buffer is shorter — a
        // slot may be the only one active and we haven't padded yet.
        if active.audio.len() < target_offset {
            active.audio.resize(target_offset, 0.0);
        }
        // Sum this chunk into the buffer. Positions past the current
        // buffer end are pushed (extending it); positions inside get
        // summed with whatever's already there — that's how a second
        // slot's speech mixes with a first slot's at the same
        // wall-clock moment. Contiguous same-slot writes never sum
        // with themselves because the cursor always sits at (or past)
        // this slot's previous end.
        let end = (target_offset + chunk.len()).min(max_samples);
        let take = end.saturating_sub(target_offset);
        for i in 0..take {
            let dst_idx = target_offset + i;
            if dst_idx >= active.audio.len() {
                active.audio.push(chunk[i]);
            } else {
                active.audio[dst_idx] += chunk[i];
            }
        }
        // Advance the slot's cursor by the frames we actually wrote
        // (may be short if we hit the RAM cap). Next chunk lands
        // exactly here — no jitter, no overlap with ourselves.
        let written_frames = (take / 2) as u64;
        active.slot_cursors[slot] = Some(target_frame + written_frames);
        Some(target_frame)
    }
}

/// Result of `RecordState::take_active`. Owned buffers so the caller can
/// hand them straight to the WAV encoder / store without holding the state
/// lock.
pub struct TakenRecording {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub entries: Vec<TranscriptEntry>,
    pub audio: Vec<f32>,
    /// Interleaved stereo mixed track, silence-gated. May be empty if
    /// nothing above the gate threshold ever landed while recording.
    pub mixed_audio: Vec<f32>,
    pub sample_rate: u32,
}

impl TakenRecording {
    pub fn duration_ms(&self) -> u64 {
        audio_duration_ms(self.audio.len(), self.sample_rate)
    }
}

/// Known SenseVoice single-word false-positive outputs. When the model
/// is handed a short, marginal segment (a mic bump, a Discord notification
/// chirp, one syllable of background noise) it often "hallucinates" one
/// of these very short bookend phrases. A real utterance of just "I." or
/// "The." isn't meaningful without the rest of the sentence, so the
/// operator loses nothing by dropping them. Matched exactly (trimmed);
/// broader stop-lists risk swallowing legitimate one-word replies.
fn is_false_positive(text: &str) -> bool {
    matches!(text.trim(), "I." | "The.")
}

fn audio_duration_ms(interleaved_len: usize, sample_rate: u32) -> u64 {
    if sample_rate == 0 {
        return 0;
    }
    let frames = (interleaved_len / 2) as u64;
    (frames * 1000) / sample_rate as u64
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Millisecond-precision unix wall-clock timestamp. Used as the
/// transcript sort key so cross-channel utterances land in the log in
/// the order they *started*, not the order their VAD/STT decode
/// finished.
fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Ordering key for a transcript entry — millisecond wall-clock of when
/// the utterance actually started. Falls back to `created_at * 1000`
/// for entries that pre-date the `start_wall_ms` field (e.g. saved
/// recordings from an older build) so mixed-vintage transcripts still
/// sort into a stable order.
fn entry_order_key(entry: &TranscriptEntry) -> u64 {
    entry
        .start_wall_ms
        .unwrap_or_else(|| (entry.created_at.max(0) as u64).saturating_mul(1000))
}

/// Insert `entry` into `entries` at the position that keeps the vector
/// sorted by `entry_order_key`. Scans from the tail (the common case
/// is "new entry belongs at or near the end") and preserves relative
/// order for ties — so two utterances that started at the same instant
/// keep the order they were pushed in.
fn insert_ordered(entries: &mut Vec<TranscriptEntry>, entry: TranscriptEntry) {
    let key = entry_order_key(&entry);
    let mut idx = entries.len();
    while idx > 0 && entry_order_key(&entries[idx - 1]) > key {
        idx -= 1;
    }
    entries.insert(idx, entry);
}

/// Commands crossing the async → blocking boundary for one slot's
/// streaming decoder. See [`run_streaming_decoder`].
enum StreamCmd {
    /// Begin a new utterance. Any prior stream state is reset first.
    Start(StreamStart),
    /// Append one chunk of the current utterance's mono audio. Ignored
    /// when no `Start` has landed yet.
    Feed(Arc<[f32]>),
    /// Close the current utterance's sherpa stream state so the next
    /// `Start` begins clean. Does NOT emit a final — the async VAD
    /// worker owns re-transcription via the offline SenseVoice model
    /// (higher accuracy on the full-clip decode) and pushes the
    /// polished final itself via [`RecordState::finalize_partial`],
    /// which replaces whatever provisional partial the streaming
    /// decoder last broadcast.
    EndUtterance,
    /// Discard the current utterance without emitting a final —
    /// removes any partial rows already broadcast for its `entry_id`.
    /// Used when the VAD closes a too-short utterance we've decided
    /// to drop as noise.
    Discard,
}

/// Metadata captured at speech-start time. `entry_id` is generated by
/// the VAD worker so partials + the eventual final share one row.
struct StreamStart {
    entry_id: String,
    slot: usize,
    audio_start_ms: Option<u64>,
    start_wall_ms: Option<u64>,
}

/// Streaming state machine — voiced vs. unvoiced, plus enough context to
/// carry pre-roll audio into the recognizer when speech starts. Runs at
/// windowed granularity so the RMS check reflects speech energy over
/// ~20 ms rather than instantaneous amplitude (voice signals oscillate
/// through zero many times per cycle and would defeat a per-sample check).
struct VadWorker {
    slot: usize,
    sample_rate: u32,
    stt: SttHandle,
    state: RecordState,
    /// mpsc sender to the per-slot blocking streaming decoder thread.
    /// `None` when streaming STT wasn't configured; VAD falls back to
    /// the offline SenseVoice finalize path (same behaviour as pre-
    /// streaming). Present = every speech-start / chunk / finalize
    /// event is mirrored into the streaming pipeline for live partials.
    streaming_tx: Option<std::sync::mpsc::SyncSender<StreamCmd>>,
    /// Id assigned at the current utterance's speech-start. Reused by
    /// every partial emission so upsert-by-id works, then dropped at
    /// finalize/discard time.
    current_entry_id: Option<String>,
    window_samples: usize,
    /// Mono samples buffered until we've got a full window's worth.
    scratch: Vec<f32>,
    /// Utterance being collected (mono). Drained when the utterance closes.
    utterance: Vec<f32>,
    /// Rolling ring of past mono samples so we can prepend the last
    /// ~PRE_ROLL_MS on Speaking transition and catch the syllable's
    /// attack. Stored as raw samples (not windows) for simplicity.
    pre_roll: std::collections::VecDeque<f32>,
    speaking: bool,
    voiced_windows: u32,
    silence_windows: u32,
    /// Timeline position (in mono frames) for the *start of the current
    /// chunk*. Updated each `process_chunk` from the recording's shared
    /// wall-clock cursor. Windows within the chunk step forward from
    /// here so that utterance_start_frame reflects wall-clock time in
    /// the recording's mixed audio buffer, even when other slots are
    /// also contributing to the same timeline.
    chunk_start_frame: u64,
    /// Cumulative mono frames advanced within the current chunk. Reset
    /// to 0 at each `process_chunk` call.
    chunk_offset_frames: u64,
    /// Snapshot of the current chunk timeline position at the
    /// Speaking-transition moment, minus the pre-roll length prepended
    /// into `utterance`. This is the utterance's start position in the
    /// recording's shared timeline.
    utterance_start_frame: u64,
    /// Wall-clock unix ms at speech-start. Used as the entry's
    /// `start_wall_ms` sort key so partials land at their true position
    /// in the log instead of jumping around as more text arrives.
    utterance_start_wall_ms: u64,
    /// Counters used purely for log breadcrumbs. Reset every second so a
    /// long-running process's log stays terse but you can still see the
    /// worker is alive + roughly how many chunks/windows it's seeing.
    log_windows: u64,
    log_hot: u64,
    log_max_rms: f32,
    log_last_report: std::time::Instant,
}

impl VadWorker {
    fn new(
        slot: usize,
        sample_rate: u32,
        stt: SttHandle,
        state: RecordState,
        streaming_tx: Option<std::sync::mpsc::SyncSender<StreamCmd>>,
    ) -> Self {
        let window_samples =
            ((sample_rate as u64 * VAD_WINDOW_MS as u64) / 1000).max(1) as usize;
        let pre_roll_capacity = (sample_rate as u64 * VAD_PRE_ROLL_MS as u64 / 1000) as usize;
        Self {
            slot,
            sample_rate,
            stt,
            state,
            streaming_tx,
            current_entry_id: None,
            window_samples,
            scratch: Vec::with_capacity(window_samples * 2),
            utterance: Vec::new(),
            pre_roll: std::collections::VecDeque::with_capacity(pre_roll_capacity),
            speaking: false,
            voiced_windows: 0,
            silence_windows: 0,
            chunk_start_frame: 0,
            chunk_offset_frames: 0,
            utterance_start_frame: 0,
            utterance_start_wall_ms: 0,
            log_windows: 0,
            log_hot: 0,
            log_max_rms: 0.0,
            log_last_report: std::time::Instant::now(),
        }
    }

    fn pre_roll_capacity(&self) -> usize {
        (self.sample_rate as u64 * VAD_PRE_ROLL_MS as u64 / 1000) as usize
    }
    fn speech_start_windows(&self) -> u32 {
        (VAD_SPEECH_START_MS / VAD_WINDOW_MS).max(1)
    }
    fn silence_end_windows(&self) -> u32 {
        (VAD_SILENCE_END_MS / VAD_WINDOW_MS).max(1)
    }
    fn max_utterance_samples(&self) -> usize {
        ((self.sample_rate as u64 * VAD_MAX_UTTERANCE_MS as u64) / 1000) as usize
    }

    /// Process one interleaved stereo chunk from vox_tap. Downmixes to
    /// mono, accumulates in `scratch`, and drives the state machine one
    /// window at a time. Dispatch to STT happens inside
    /// `finalize_utterance` when the silence-end trigger fires.
    ///
    /// `timeline_frame` is the recording's wall-clock frame offset where
    /// this chunk was mixed into the shared audio buffer. When no
    /// recording is active, callers pass `None` and the worker falls
    /// back to advancing its own counter — utterance offsets in that
    /// case are meaningless (there's no recording to seek into) but the
    /// VAD still functions for the rolling ephemeral buffer.
    fn process_chunk(&mut self, stereo: &[f32], timeline_frame: Option<u64>) {
        if let Some(f) = timeline_frame {
            self.chunk_start_frame = f;
        } else {
            // No active recording — advance a private counter so the
            // fallback offsets are at least monotonic per-worker.
            self.chunk_start_frame = self
                .chunk_start_frame
                .saturating_add(self.chunk_offset_frames);
        }
        self.chunk_offset_frames = 0;

        self.scratch.reserve(stereo.len() / 2);
        for pair in stereo.chunks_exact(2) {
            self.scratch.push((pair[0] + pair[1]) * 0.5);
        }
        while self.scratch.len() >= self.window_samples {
            let window: Vec<f32> = self.scratch.drain(..self.window_samples).collect();
            self.process_window(&window);
        }
    }

    fn process_window(&mut self, window: &[f32]) {
        // Advance within the current chunk's timeline slice. `end_frame`
        // is the position (in mono frames on the shared recording
        // timeline) just past this window's last sample.
        self.chunk_offset_frames =
            self.chunk_offset_frames.saturating_add(window.len() as u64);
        let end_frame = self
            .chunk_start_frame
            .saturating_add(self.chunk_offset_frames);

        // RMS over the window. `sum_sq / n` then sqrt — enough precision
        // in f32 for the range of amplitudes we care about.
        let rms = if window.is_empty() {
            0.0
        } else {
            let sum_sq: f32 = window.iter().map(|s| s * s).sum();
            (sum_sq / window.len() as f32).sqrt()
        };
        let hot = rms > VAD_RMS_THRESHOLD;
        self.log_windows += 1;
        if hot {
            self.log_hot += 1;
        }
        if rms > self.log_max_rms {
            self.log_max_rms = rms;
        }
        if self.log_last_report.elapsed() >= std::time::Duration::from_secs(5) {
            debug!(
                channel = %self.state.channel_name(self.slot),
                windows = self.log_windows,
                hot = self.log_hot,
                max_rms = self.log_max_rms,
                threshold = VAD_RMS_THRESHOLD,
                speaking = self.speaking,
                "vad breadcrumb"
            );
            self.log_windows = 0;
            self.log_hot = 0;
            self.log_max_rms = 0.0;
            self.log_last_report = std::time::Instant::now();
        }

        if self.speaking {
            self.utterance.extend_from_slice(window);
            // Mirror this window into the streaming decoder if
            // configured. Sherpa consumes any chunk size, so per-
            // window (~20 ms) is fine — the blocking decoder can
            // saturate a core if needed and its mpsc channel absorbs
            // bursts. `try_send` avoids blocking the tokio task on
            // channel-full; a dropped feed just means one 20ms slice
            // is skipped from the sherpa input, which is imperceptible.
            if let Some(tx) = &self.streaming_tx {
                let _ = tx.try_send(StreamCmd::Feed(Arc::from(window.to_vec())));
            }
            if hot {
                self.silence_windows = 0;
            } else {
                self.silence_windows = self.silence_windows.saturating_add(1);
                if self.silence_windows >= self.silence_end_windows() {
                    self.finalize_utterance("silence");
                    return;
                }
            }
            if self.utterance.len() >= self.max_utterance_samples() {
                self.finalize_utterance("max-length");
            }
        } else {
            // Idle. Roll the pre-roll ring forward so we always have the
            // last ~PRE_ROLL_MS of mono audio ready to prepend on trigger.
            let cap = self.pre_roll_capacity();
            if cap > 0 {
                for &s in window {
                    if self.pre_roll.len() >= cap {
                        self.pre_roll.pop_front();
                    }
                    self.pre_roll.push_back(s);
                }
            }
            if hot {
                self.voiced_windows = self.voiced_windows.saturating_add(1);
                if self.voiced_windows >= self.speech_start_windows() {
                    self.speaking = true;
                    self.silence_windows = 0;
                    self.utterance.clear();
                    // Drain the pre-roll into the utterance so sherpa
                    // sees the syllable's attack. Deque is empty after
                    // and ready to refill once we're idle again.
                    self.utterance.extend(self.pre_roll.drain(..));
                    // Utterance's leading edge in the recording's shared
                    // timeline: end-of-current-window minus the pre-roll
                    // length we just prepended into `utterance`. Clamped
                    // so early triggers (before pre_roll has filled)
                    // don't underflow.
                    self.utterance_start_frame = end_frame
                        .saturating_sub(self.utterance.len() as u64);
                    self.utterance_start_wall_ms = unix_now_ms();
                    debug!(
                        channel = %self.state.channel_name(self.slot),
                        rms,
                        pre_roll = self.utterance.len(),
                        start_frame = self.utterance_start_frame,
                        "vad: speaking",
                    );
                    // Kick off a streaming session for this utterance.
                    // The pre-roll (already in `utterance`) is sent as
                    // the first Feed so sherpa sees the syllable
                    // attack, matching what the offline path decodes.
                    if let Some(tx) = &self.streaming_tx {
                        let entry_id = Uuid::new_v4().to_string();
                        self.current_entry_id = Some(entry_id.clone());
                        let start_ms = self.utterance_start_frame * 1000
                            / self.sample_rate as u64;
                        let _ = tx.try_send(StreamCmd::Start(StreamStart {
                            entry_id,
                            slot: self.slot,
                            audio_start_ms: Some(start_ms),
                            start_wall_ms: Some(self.utterance_start_wall_ms),
                        }));
                        if !self.utterance.is_empty() {
                            let pre: Arc<[f32]> = Arc::from(self.utterance.clone());
                            let _ = tx.try_send(StreamCmd::Feed(pre));
                        }
                    }
                }
            } else {
                self.voiced_windows = 0;
            }
        }
    }

    /// Close the current utterance. The streaming decoder (if configured)
    /// has been broadcasting live partials for the in-flight text, but
    /// the offline SenseVoice model produces a materially more accurate
    /// full-clip decode — so at close time we throw away the streaming
    /// final and run offline STT over the accumulated samples, then push
    /// its output as the entry's polished text.
    ///
    /// * Streaming enabled — send an `EndUtterance` to reset the
    ///   per-slot sherpa session (so the next `Start` begins clean),
    ///   then run offline SenseVoice and replace the provisional
    ///   partial via `state.finalize_partial(entry_id, ...)`.
    /// * Streaming disabled — same offline decode, pushed via
    ///   `state.push_transcript(...)` because there's no partial row
    ///   to replace. Matches the pre-streaming behaviour.
    fn finalize_utterance(&mut self, reason: &'static str) {
        let samples = std::mem::take(&mut self.utterance);
        let start_frame = self.utterance_start_frame;
        let start_wall_ms = self.utterance_start_wall_ms;
        let current_entry_id = self.current_entry_id.take();
        self.speaking = false;
        self.silence_windows = 0;
        self.voiced_windows = 0;
        self.pre_roll.clear();
        let sr = self.sample_rate;
        let state = self.state.clone();
        let slot = self.slot;
        let len = samples.len();
        // Below ~200 ms of audio is almost always noise (a lone loud
        // sample that briefly cleared the RMS threshold); skip the decode
        // to save CPU rather than emit an empty transcript.
        if len < (sr as usize / 5) {
            debug!(
                channel = %state.channel_name(slot),
                reason, len,
                "vad: dropping tiny utterance"
            );
            // Also discard the streaming session's in-flight state so
            // its next Start begins clean, and remove any partial rows
            // we broadcast for this now-abandoned utterance.
            if let Some(tx) = &self.streaming_tx {
                let _ = tx.try_send(StreamCmd::Discard);
            }
            if let Some(id) = &current_entry_id {
                state.remove_entry(id);
            }
            return;
        }
        let start_ms = start_frame * 1000 / sr as u64;
        let duration_ms = (len as u64) * 1000 / sr as u64;
        // Snapshot the speaker attribution NOW, at the moment the VAD
        // decided the utterance ended — not after STT decode returns
        // (which can take hundreds of ms and would let a later hint
        // steal attribution). Per the design: "identification should
        // be determined by relative timing at the END of each clip".
        let end_instant = Instant::now();
        let speaker = state.resolve_speaker(slot, end_instant);
        debug!(
            channel = %state.channel_name(slot),
            reason, len, start_ms, duration_ms,
            speaker = ?speaker,
            "vad: finalizing utterance"
        );

        // Reset the streaming sherpa session so the next Start begins
        // clean. The last streaming partial stays visible (still
        // provisional) until the offline decode below replaces it.
        if let Some(tx) = &self.streaming_tx {
            let _ = tx.try_send(StreamCmd::EndUtterance);
        }

        // Offline SenseVoice re-transcribe. Fires-and-forgets so the
        // VAD loop can start the next utterance immediately; the
        // callback lands the polished text via either
        // `finalize_partial` (streaming path — replaces the row keyed
        // by entry_id) or `push_transcript` (streaming disabled —
        // insert as a fresh row).
        let stt = self.stt.clone();
        let start_wall_ms = if start_wall_ms == 0 {
            unix_now_ms().saturating_sub(duration_ms)
        } else {
            start_wall_ms
        };
        tokio::spawn(async move {
            let res = tokio::task::spawn_blocking(move || stt.transcribe(&samples, sr)).await;
            match res {
                Ok(Ok(text)) => {
                    let trimmed = text.trim().to_string();
                    if let Some(entry_id) = current_entry_id {
                        // Streaming path: replace the provisional row
                        // (id-keyed) with the offline final. The same
                        // false-positive filter as `push_transcript`
                        // applies inside `finalize_partial` — a junk
                        // decode removes the row rather than promoting
                        // it.
                        state.finalize_partial(
                            &entry_id,
                            slot,
                            trimmed,
                            Some(start_ms),
                            Some(duration_ms),
                            speaker,
                            Some(start_wall_ms),
                        );
                    } else if trimmed.chars().any(|c| c.is_alphanumeric())
                        && !is_false_positive(&trimmed)
                    {
                        state.push_transcript(
                            slot,
                            trimmed,
                            Some(start_ms),
                            Some(duration_ms),
                            speaker,
                            Some(start_wall_ms),
                        );
                    }
                }
                Ok(Err(err)) => {
                    warn!(err = %format!("{err:#}"), "offline STT decode failed");
                }
                Err(_) => {
                    warn!("offline STT task panicked");
                }
            }
        });
    }
}

/// Per-slot blocking loop that owns the [`StreamingSession`]. Runs on
/// `spawn_blocking` so its synchronous sherpa calls don't stall async
/// tokio workers. Commands arrive via `std::sync::mpsc` from the async
/// VAD worker.
///
/// One utterance = one `Start` + zero-or-more `Feed`s + one
/// `EndUtterance` (or `Discard`). Partial hypotheses are pushed via
/// `state.upsert_partial` after each Feed that materially changes the
/// text; on `EndUtterance` the sherpa stream is reset but no final is
/// emitted — the async VAD worker owns re-transcription via the
/// offline (higher-accuracy) SenseVoice model and replaces the last
/// partial itself.
fn run_streaming_decoder(
    session: StreamingSession,
    rx: std::sync::mpsc::Receiver<StreamCmd>,
    state: RecordState,
    sample_rate: u32,
) {
    let mut current: Option<StreamStart> = None;
    let mut last_partial = String::new();
    let mut last_emit = Instant::now();
    // Minimum wall-clock time between partial broadcasts. Keeps SSE
    // fan-out and the recording snapshot polls from thrashing when the
    // recognizer emits token-per-chunk; ~150 ms feels responsive but
    // stays comfortably under the client's ~500 ms poll cadence.
    let min_emit_interval = std::time::Duration::from_millis(150);
    while let Ok(cmd) = rx.recv() {
        match cmd {
            StreamCmd::Start(start) => {
                session.reset();
                last_partial.clear();
                last_emit = Instant::now();
                current = Some(start);
            }
            StreamCmd::Feed(samples) => {
                let Some(start) = current.as_ref() else { continue; };
                session.feed(&samples, sample_rate);
                let text = session.current_text();
                if text.is_empty() {
                    continue;
                }
                let now = Instant::now();
                let changed = text != last_partial;
                let debounce_ok = now.duration_since(last_emit) >= min_emit_interval
                    || last_partial.is_empty();
                if changed && debounce_ok {
                    state.upsert_partial(
                        &start.entry_id,
                        start.slot,
                        text.clone(),
                        start.audio_start_ms,
                        None, // duration not yet known
                        None, // speaker resolved at finalize
                        start.start_wall_ms,
                    );
                    last_partial = text;
                    last_emit = now;
                }
            }
            StreamCmd::EndUtterance => {
                if current.take().is_some() {
                    // Just reset the stream; discard whatever text the
                    // online decoder would have produced — the async
                    // side is running offline SenseVoice over the full
                    // utterance and its output will replace the last
                    // provisional partial we broadcast.
                    session.reset();
                }
                last_partial.clear();
                last_emit = Instant::now();
            }
            StreamCmd::Discard => {
                session.reset();
                if let Some(start) = current.take() {
                    state.remove_entry(&start.entry_id);
                }
                last_partial.clear();
                last_emit = Instant::now();
            }
        }
    }
    info!("streaming decoder channel closed; exiting");
}

/// Spawn one streaming VAD worker per Vox slot. Each worker subscribes to
/// its own broadcast tap and, at each utterance finalize, snapshots the
/// current mixer slot state to decide whether it's enabled. Disabled
/// slots still drain their tap chunks (to keep the broadcast lag counter
/// happy) but skip the VAD state machine entirely — no CPU spent on
/// windows or decodes for capture channels that aren't in use.
///
/// The `active` recording (when one exists) receives audio from every
/// worker's tap; this keeps the recording faithful to whatever the
/// operator has enabled at the moment of `POST /record/recordings`.
///
/// Silently no-ops when `stt` is `None` so callers can wire this in
/// unconditionally.
pub fn spawn_worker(
    stt: Option<SttHandle>,
    streaming: Option<StreamingSttHandle>,
    vox_taps: Vec<broadcast::Sender<Arc<[f32]>>>,
    sample_rate: u32,
    state: RecordState,
) {
    let Some(stt) = stt else {
        info!("record streaming workers not started (STT disabled)");
        return;
    };
    for (slot, tap) in vox_taps.into_iter().enumerate() {
        let mut rx = tap.subscribe();
        let stt = stt.clone();
        let streaming = streaming.clone();
        let state = state.clone();
        tokio::spawn(async move {
            // Spin up the per-slot blocking decoder if streaming is
            // configured. Bounded mpsc so a stalled decoder can't
            // buffer unbounded audio — `try_send` on the async side
            // drops on backpressure, which is the right trade-off for
            // real-time text (an occasional missed 20 ms sherpa feed
            // is imperceptible in the final transcript).
            let streaming_tx = streaming.map(|handle| {
                let (tx, rx) = std::sync::mpsc::sync_channel::<StreamCmd>(256);
                let session = handle.new_session();
                let state = state.clone();
                tokio::task::spawn_blocking(move || {
                    run_streaming_decoder(session, rx, state, sample_rate);
                });
                tx
            });
            let has_streaming = streaming_tx.is_some();
            let mut worker = VadWorker::new(
                slot,
                sample_rate,
                stt,
                state.clone(),
                streaming_tx,
            );
            info!(
                slot,
                sample_rate,
                streaming = has_streaming,
                "record streaming worker started"
            );
            loop {
                match rx.recv().await {
                    Ok(chunk) => {
                        // Only the enabled slot(s) contribute audio to the
                        // active recording — otherwise a disabled capture
                        // channel would leak into every named recording.
                        let enabled = state
                            .mixer
                            .vox_slot(slot)
                            .map(|s| s.enabled())
                            .unwrap_or(false);
                        if enabled {
                            // Mix this slot's chunk into the shared
                            // recording buffer at its wall-clock offset.
                            // `push_audio` returns None when nothing is
                            // being recorded; in that case we still run
                            // the VAD (for the rolling ephemeral buffer)
                            // but the utterance offsets it stamps have
                            // no recording to seek into.
                            let timeline_frame = state.push_audio(slot, &chunk);
                            worker.process_chunk(&chunk, timeline_frame);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(slot, missed = n, "record worker lagged on vox tap");
                        // Discard any in-flight streaming state too, so
                        // sherpa doesn't glue post-lag audio onto the
                        // pre-lag partial and produce a garbled entry.
                        if let Some(tx) = &worker.streaming_tx {
                            let _ = tx.try_send(StreamCmd::Discard);
                        }
                        if let Some(id) = worker.current_entry_id.take() {
                            worker.state.remove_entry(&id);
                        }
                        worker.utterance.clear();
                        worker.scratch.clear();
                        worker.speaking = false;
                        worker.silence_windows = 0;
                        worker.voiced_windows = 0;
                        worker.pre_roll.clear();
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        info!(slot, "vox tap closed; record worker exiting");
                        return;
                    }
                }
            }
        });
    }
}
