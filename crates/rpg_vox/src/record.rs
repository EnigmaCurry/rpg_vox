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
//! * **Not Recording** — every clip is appended to a rolling per-channel
//!   paragraph log; each channel is capped at [`CHANNEL_PARAGRAPH_CAP`]
//!   paragraphs and older paragraphs drop FIFO when the cap is exceeded.
//! * **Recording** — a named "bucket" is created on `POST /record/recordings`.
//!   Every subsequent clip is written to both the per-channel logs AND the
//!   active bucket, and every raw stereo audio chunk is also appended to the
//!   bucket so the eventual `/stop` can persist a WAV + paragraph JSON.
//!
//! Paragraph model (Stage 1 of the paragraph refactor):
//!   * Each channel (one per Vox slot + one TTS pseudo-channel) has an
//!     ordered `Vec<Paragraph>`. A paragraph is a growing run of
//!     [`ClipRef`]s that were captured on the same channel within
//!     [`PARAGRAPH_GAP_MS`] of each other. Once the gap between a new clip
//!     and the previous clip's end exceeds that threshold a new paragraph
//!     opens.
//!   * `Paragraph.text` is a naïve space-join of its clips' texts (Stage
//!     1 has no LLM reorganization — pass 3 + pass 4 land in later stages).
//!
//! When STT is disabled at startup no worker spawns and no transcripts land;
//! the record HTTP endpoints still work (the UI stays empty) so future
//! recognizer additions can drop in without further wiring.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::mixer::AtomicMixer;
use crate::paragraph::LlmScheduler;
use crate::stt::{StreamingSession, StreamingSttHandle, SttHandle};

/// Silence gap (ms) that forces a paragraph break on a given channel. A
/// new clip whose start is more than this far past the previous clip's end
/// (on the same channel) starts a fresh paragraph instead of appending to
/// the growing one.
pub const PARAGRAPH_GAP_MS: u64 = 2_000;

/// Absolute ceiling (ms since last clip end) after which a paragraph
/// is force-hardened regardless of outstanding pass-4 work. Prevents a
/// stuck / down LLM from leaving paragraphs perpetually un-hardened in
/// the UI. Chosen well past a typical LLM latency (a few seconds) but
/// short enough that a wedged pipeline surfaces to the operator within
/// a reasonable window.
pub const HARDEN_MAX_WAIT_MS: u64 = 30_000;

/// Soft cap on paragraph length (in words). Once a paragraph exceeds
/// this AND its most recent clip ends on a sentence-terminating
/// punctuation mark, the next clip on the channel opens a fresh
/// paragraph. Prevents runaway "one giant block" paragraphs during
/// non-stop speech (McKenna case). The cut is always between clips,
/// so the new paragraph's `start_wall_ms` is the exact timestamp of
/// its first clip — no timing guesswork. If the most recent clip
/// doesn't end on a sentence marker (SenseVoice occasionally omits
/// punctuation), the paragraph keeps growing until a punctuated clip
/// lands. Absolutely no mid-clip cuts.
///
/// Tuned for short readable paragraphs — ~100 words is a few
/// sentences, breaks cleanly on the next natural pause.
pub const PARAGRAPH_SOFT_MAX_WORDS: usize = 100;

/// Hard cap on paragraph length (in words). Once a paragraph exceeds
/// this on the next clip finalize, it force-closes regardless of
/// whether a natural boundary (silence-close, pass-3 punctuation) was
/// available. Prevents non-stop-speech scenarios (McKenna monologue
/// where every clip is a VAD max-length rollover with no perceptible
/// pauses) from producing endless single-block paragraphs. Set at 2×
/// the soft cap so the preferred natural-boundary path still gets
/// several clips' worth of headroom before the hard fallback fires.
pub const PARAGRAPH_HARD_MAX_WORDS: usize = 200;

/// Longest single named recording we buffer in RAM before force-closing it.
/// 30 min at 48 kHz stereo f32 ≈ 690 MB — big but survivable, and past that
/// the operator almost certainly forgot to stop.
const RECORDING_MAX_FRAMES: usize = 48_000 * 60 * 30;

/// Retention window for the per-slot mono audio ring. Big enough to cover
/// the last few clips plus a safety margin so pass 3 can re-transcribe
/// the last 2–3 clips without ever missing audio. 90 s at 48 kHz mono
/// f32 ≈ 17 MB per slot — well within budget for the small vox slot
/// count.
const AUDIO_RING_RETENTION_MS: u64 = 90_000;
/// Cap on how many mono samples a pass-3 window may cover. SenseVoice
/// degrades on inputs much longer than ~25 s; keep the window under it.
const BOUNDARY_MAX_WINDOW_MS: u64 = 25_000;
/// Minimum wall-clock gap between successive pass-3 fires per paragraph.
/// Cheap safety net so a rapid succession of `finalize_clip` calls
/// doesn't stack blocking STT decodes on the spawn_blocking pool.
const BOUNDARY_DEBOUNCE_MS: u64 = 500;
/// Max silence gap between the borrowed previous-paragraph clip and
/// the current paragraph's first clip. Above this we skip the
/// cross-paragraph borrow entirely — a long pause means the speaker
/// took a real break, so there's no continuous-speech seam to smooth
/// and feeding SenseVoice audio spanning a long silence tends to
/// make it drop content on one side (which then corrupts
/// previously-good text via the proportional split).
const BOUNDARY_BORROW_MAX_GAP_MS: u64 = 3_000;

/// Cap on paragraphs kept per channel outside of a named recording. Older
/// paragraphs drop FIFO when this is exceeded so the rolling per-channel
/// log can't grow unbounded across a long-running session.
const CHANNEL_PARAGRAPH_CAP: usize = 200;

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

/// Pseudo-channel name used for TTS paragraphs. Every TTS clip lands on
/// this channel regardless of which voice profile spoke — the UI keys off
/// it to render TTS separately from vox capture channels.
pub const TTS_CHANNEL_NAME: &str = "TTS";

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

/// Which channel a [`Paragraph`] belongs to. `Vox(slot)` is a specific
/// mixer capture slot; `Tts` is the TTS pseudo-channel that hosts every
/// TTS playback as a single-clip paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelKind {
    Vox(usize),
    Tts,
}

/// One clip (a VAD-closed utterance, or a TTS playback) inside a
/// [`Paragraph`]. `text` is whatever the recognizer produced (or the TTS
/// text for TTS clips). `provisional=true` while a streaming partial is
/// still growing; flips to false when the offline SenseVoice final lands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipRef {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_start_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_duration_ms: Option<u64>,
    pub start_wall_ms: u64,
    pub text: String,
    #[serde(default)]
    pub provisional: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mixed_start_ms: Option<u64>,
    /// True once this specific clip's text has been rewritten by pass 3
    /// (boundary re-transcription of a rolling multi-clip window through
    /// SenseVoice). Read by the paragraph soft-cap check so we only cut
    /// a paragraph at a clip whose trailing punctuation reflects a real
    /// sentence end, not the fake period SenseVoice always tacks onto
    /// its per-clip decodes. Preserved across LLM diff cycles since the
    /// LLM only rewrites paragraph text, not per-clip text.
    #[serde(default)]
    pub pass3_ran: bool,
    /// True once pass 4 (the LLM conservative-corrector) has run on
    /// this clip. Pass 4 is per-clip in the current design — it
    /// applies dictionary/vocabulary corrections and can rewrite/merge
    /// sentences within a clip for readability. Skipped for clips
    /// that already have `pass4_ran=true` so we don't burn LLM budget
    /// re-processing text that was already polished.
    #[serde(default)]
    pub pass4_ran: bool,
}

/// One user-authored correction stored as a patch on top of the
/// paragraph transcription. Never mutates `Paragraph::text` /
/// `raw_text` — the client applies the patch at render time by
/// substring-replacing `original` with `replacement` and painting the
/// swap with a green underline. When the underlying transcription
/// drifts (LLM rewrites, pass-3 boundaries) the edit is silently
/// orphaned if `original` no longer occurs in `text`. We preserve
/// orphans so a subsequent revision that re-introduces the original
/// phrasing picks the edit back up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParagraphEdit {
    pub id: String,
    pub original: String,
    pub replacement: String,
    pub created_at: i64,
}

/// A run of same-channel clips grouped by silence-gap. Stage 1 keeps
/// `text` as a naive space-join of `clips[].text`; later stages will let
/// the LLM reshape paragraph text. `hardened=false` for vox paragraphs
/// throughout Stage 1 — paragraph hardening is a Stage 3 concern.
/// TTS single-clip paragraphs are hardened immediately.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Paragraph {
    pub id: String,
    pub channel: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    pub hardened: bool,
    pub text: String,
    pub raw_text: String,
    pub clips: Vec<ClipRef>,
    pub start_wall_ms: u64,
    pub end_wall_ms: u64,
    pub created_at: i64,
    /// True once pass-3 boundary re-transcription has completed at
    /// least one cycle for this paragraph. Persisted so a re-loaded
    /// recording keeps its "condensed" status.
    #[serde(default)]
    pub pass3_ran: bool,
    /// True once the LLM has emitted this paragraph (pass 4). Preserved
    /// across further LLM cycles; a paragraph split into new siblings
    /// starts fresh (false) so the new paragraphs get their own state.
    #[serde(default)]
    pub pass4_ran: bool,
    /// Transient: set while a pass-3 spawn_blocking decode is in flight
    /// for this paragraph. Cleared on completion or abort. Not persisted.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pass3_inflight: bool,
    /// Soft-cap boundary marker. Set by the pass-3 completion path when
    /// a paragraph exceeds `PARAGRAPH_SOFT_MAX_WORDS` AND its most
    /// recent pass-3-authored clip ends on a sentence-terminating
    /// punctuation mark. The next clip arriving on the channel treats
    /// this like a silence-gap break and opens a fresh paragraph, even
    /// though no real silence occurred. Prevents runaway one-block
    /// paragraphs on non-stop speech without touching clip timing.
    #[serde(default)]
    pub closed: bool,
    /// User-authored corrections stored as an overlay on top of the
    /// transcription. See [`ParagraphEdit`] for the semantics.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edits: Vec<ParagraphEdit>,
    /// Soft-delete flag set by the trash-can affordance in the UI.
    /// The paragraph stays in the underlying store (so an undelete
    /// path is trivially available later), but the client skips it
    /// when rendering the transcript log.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
}

impl Paragraph {
    /// Recompute `text` + `raw_text` + `end_wall_ms` from the current
    /// clip list. Empty clips (post-filter drops) collapse the paragraph
    /// to empty strings; the caller is expected to remove such paragraphs.
    fn rebuild(&mut self) {
        let joined = self
            .clips
            .iter()
            .map(|c| c.text.as_str())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        self.text = joined.clone();
        self.raw_text = joined;
        if let Some(last) = self.clips.last() {
            let end = last
                .audio_duration_ms
                .map(|d| last.start_wall_ms.saturating_add(d))
                .unwrap_or(last.start_wall_ms);
            self.end_wall_ms = end;
        }
    }
}

/// Per-channel bounded ring of mono audio, used by pass-3 boundary
/// re-transcription. Populated unconditionally from every incoming vox
/// chunk (regardless of the enable flag or recording state) so a
/// paragraph consolidation always has audio to draw from, even in the
/// ephemeral (not-recording) case.
///
/// The ring keeps at least [`AUDIO_RING_RETENTION_MS`] of audio at
/// `sample_rate`; older samples get popped off the front once retention
/// is exceeded and `head_wall_ms` advances to keep the wall-clock
/// mapping accurate.
///
/// On a `broadcast::Lagged` (or any other observed discontinuity) the
/// ring is marked `lagged` — while set, [`Self::samples_between`]
/// returns `None` because we can't guarantee contiguity across the gap.
/// The flag clears on the next push, which also re-anchors the ring.
struct SlotAudioRing {
    /// Mono f32 samples, oldest-first. Capacity chosen so retention ≥
    /// AUDIO_RING_RETENTION_MS at sample_rate.
    samples: VecDeque<f32>,
    sample_rate: u32,
    /// Wall-clock unix-ms of the sample that would be at index 0 if the
    /// ring were reindexed from zero. Advances as older samples get
    /// popped off the front. `None` until the first push lands.
    head_wall_ms: Option<u64>,
    /// Set to `true` on a broadcast::Lagged (or any other discontinuity).
    /// While `true`, `samples_between` returns None regardless of range
    /// because we can't guarantee the ring is contiguous. Cleared on
    /// clear() or once we've observed a fresh anchor.
    lagged: bool,
}

impl SlotAudioRing {
    fn new(sample_rate: u32) -> Self {
        // Reserve enough space that steady-state pushes never realloc.
        let cap = if sample_rate == 0 {
            0
        } else {
            ((sample_rate as u64 * AUDIO_RING_RETENTION_MS) / 1000) as usize
        };
        Self {
            samples: VecDeque::with_capacity(cap),
            sample_rate,
            head_wall_ms: None,
            lagged: false,
        }
    }

    /// Push a new mono chunk. Establishes head_wall_ms on the first push,
    /// trims old samples once retention is exceeded (advancing head_wall_ms
    /// proportionally). Clears the `lagged` flag if set — the new push
    /// re-anchors the ring.
    fn push(&mut self, chunk: &[f32], wall_now_ms: u64) {
        if self.sample_rate == 0 || chunk.is_empty() {
            return;
        }
        let chunk_ms = ((chunk.len() as u64) * 1000) / self.sample_rate as u64;
        if self.head_wall_ms.is_none() || self.lagged {
            // First push, or first push after a lag — re-anchor so the
            // head reflects the wall-clock of the sample that will sit
            // at index 0 after this push lands.
            //
            // Simpler shape than the plan's "end-of-chunk anchor" wording:
            // set head_wall_ms so that after appending this chunk, the
            // sample at index `samples.len()` (past the tail) corresponds
            // to `wall_now_ms`. Equivalently, head = wall_now_ms - chunk_ms.
            self.samples.clear();
            self.head_wall_ms = Some(wall_now_ms.saturating_sub(chunk_ms));
            self.lagged = false;
        }
        self.samples.extend(chunk.iter().copied());
        // Trim retention. Keep at most AUDIO_RING_RETENTION_MS of samples;
        // advance head_wall_ms by the number of samples we drop.
        let max_samples =
            ((self.sample_rate as u64 * AUDIO_RING_RETENTION_MS) / 1000) as usize;
        if self.samples.len() > max_samples {
            let drop = self.samples.len() - max_samples;
            self.samples.drain(..drop);
            let drop_ms = ((drop as u64) * 1000) / self.sample_rate as u64;
            if let Some(head) = self.head_wall_ms.as_mut() {
                *head = head.saturating_add(drop_ms);
            }
        }
    }

    /// Mark the ring as post-Lagged. Subsequent samples_between() returns
    /// None until the next push re-anchors. Simplest: clear the ring so
    /// stale samples can't be quietly returned as if contiguous.
    fn on_lagged(&mut self) {
        self.clear();
        self.lagged = true;
    }

    /// Explicit clear (used by on_lagged and by future callers).
    fn clear(&mut self) {
        self.samples.clear();
        self.head_wall_ms = None;
        self.lagged = false;
    }

    /// Return the mono samples covering `[start_wall_ms, end_wall_ms]`
    /// inclusive on both edges. Returns `None` when the range crosses a
    /// discontinuity, predates the retained head, is entirely past the
    /// tail, or has `start > end`.
    fn samples_between(&self, start_wall_ms: u64, end_wall_ms: u64) -> Option<Vec<f32>> {
        if self.lagged || self.sample_rate == 0 || start_wall_ms > end_wall_ms {
            return None;
        }
        let head = self.head_wall_ms?;
        if start_wall_ms < head {
            // Requested window predates what we still have.
            return None;
        }
        let sr = self.sample_rate as u64;
        // Convert the requested wall range to sample indices relative to
        // the ring's head.
        let start_idx = (((start_wall_ms - head) * sr) / 1000) as usize;
        // End is inclusive: include the sample at end_wall_ms itself.
        let end_idx = ((((end_wall_ms - head) * sr) / 1000) as usize).saturating_add(1);
        if start_idx >= self.samples.len() {
            return None;
        }
        let end_idx = end_idx.min(self.samples.len());
        if end_idx <= start_idx {
            return None;
        }
        // VecDeque may be split across two contiguous slices — collect
        // via iter().skip/take rather than assuming a single slice.
        let out: Vec<f32> = self
            .samples
            .iter()
            .skip(start_idx)
            .take(end_idx - start_idx)
            .copied()
            .collect();
        Some(out)
    }
}

/// One contiguous stretch of non-silent audio written into the mixed
/// track. Recorded as we go so any wall-clock timestamp (e.g. a
/// clip's `start_wall_ms`) can be mapped to a position in
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
    /// Paragraphs captured during this recording window, ordered by
    /// arrival on any channel. When a clip finalizes inside a paragraph
    /// that already exists in `paragraphs`, the update mirrors here by id.
    paragraphs: Vec<Paragraph>,
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

/// One channel's paragraph log. Vox slots each get one; TTS gets a
/// dedicated pseudo-channel at the trailing index.
struct ChannelState {
    kind: ChannelKind,
    /// Channel display name snapshotted at boot from the mixer. Stage 1
    /// still reads the live mixer name inside `snapshot()` so the UI
    /// picks up renames without a restart — this field is retained as a
    /// stable label for Stages 2/3 (LLM scheduler + audio ring) which
    /// will key logs and worker tasks by it.
    #[allow(dead_code)]
    channel_name: String,
    paragraphs: Vec<Paragraph>,
    /// Per-channel ambient audio ring for pass-3 boundary re-transcription.
    /// Fed from the same broadcast::Recv loop that drives VAD. Populated
    /// unconditionally (not gated on named-recording state) so paragraph
    /// consolidation works in the ephemeral case too. TTS channels
    /// never receive audio pushes — the ring stays empty and pass 3
    /// short-circuits when it can't get samples.
    audio_ring: SlotAudioRing,
    /// Transient flag set by the LLM scheduler task while a pass-4
    /// (hot-zone reorganization) call is in flight for this channel.
    /// The UI reads it out of the snapshot to pulse a "reorganizing…"
    /// indicator on the channel's non-hardened paragraphs.
    llm_inflight: bool,
}

/// Serialized shape of one channel's paragraphs for the `/record`
/// snapshot. The channel name at snapshot time is a live read of the
/// mixer (so a rename shows up immediately in the UI) — historical
/// paragraphs keep the name that was captured at push time.
#[derive(Debug, Clone, Serialize)]
pub struct ChannelParagraphs {
    pub channel: String,
    pub paragraphs: Vec<Paragraph>,
    /// True while a pass-4 (LLM) call is in flight for this channel.
    /// The UI pulses the reorganizing indicator on non-hardened
    /// paragraphs when this is true. Vox-only; TTS channel stays false.
    #[serde(default)]
    pub llm_inflight: bool,
}

/// Snapshot payload for `GET /record`. Serde-serialized directly; no
/// intermediate DTO. Fields match what the Svelte page expects.
#[derive(Debug, Clone, Serialize)]
pub struct RecordStateSnapshot {
    pub mode: &'static str,
    pub paragraphs_by_channel: Vec<ChannelParagraphs>,
    /// `None` when the process is in Not Recording mode. Serialized as
    /// `null` on the wire; the UI keys off it to switch the top-of-page
    /// affordances.
    pub active_recording: Option<ActiveRecordingSnapshot>,
    /// Display names for every configured vox slot, indexed 0..N. The UI
    /// uses this as the canonical channel list.
    pub channel_names: Vec<String>,
    /// Whether pass-4 LLM cycles run while no named recording is active.
    /// Off by default; the UI toggles it via `PUT /record/llm-when-idle`.
    pub llm_when_idle: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActiveRecordingSnapshot {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub paragraphs: Vec<Paragraph>,
    /// Approximate seconds captured so far, computed from the audio buffer
    /// length. Rough enough to drive a live "recording: 0:42" counter.
    pub duration_ms: u64,
    /// Duration (ms) of the silence-gated mixed track. Roughly what
    /// pressing "▶ Mixed" would take to play. Almost always shorter than
    /// `duration_ms` because of the silence trim.
    pub mixed_duration_ms: u64,
}

struct Inner {
    /// One entry per vox slot (indexed by slot) followed by one trailing
    /// entry for the TTS pseudo-channel. Initialized once at construction
    /// time; the vector's shape is stable for the process's lifetime.
    channels: Vec<ChannelState>,
    active: Option<ActiveRecording>,
    /// Per-paragraph wall-clock timestamp of the last pass-3 fire.
    /// Consulted by `schedule_boundary_retranscribe` to skip runs that
    /// would fire within [`BOUNDARY_DEBOUNCE_MS`] of the previous one.
    /// Entries are keyed by paragraph id — a paragraph that's since been
    /// removed leaves a stale entry behind (harmless; cheap and self-
    /// bounded by the paragraph cap).
    boundary_last_fire_ms: HashMap<String, u64>,
}

impl Inner {
    /// Find the channel state for the given kind. Returns `None` when the
    /// slot index is out of range — an unusual case but not fatal.
    fn channel_mut(&mut self, kind: ChannelKind) -> Option<&mut ChannelState> {
        self.channels.iter_mut().find(|c| c.kind == kind)
    }
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
    /// slot's display name. Snapshots go onto each paragraph at capture
    /// time so historical entries keep their at-capture label even after
    /// a subsequent rename.
    mixer: Arc<AtomicMixer>,
    /// Bounded ring of recent speaker hints from external sources (e.g.
    /// discord_vox POSTing to /record/hint). Consulted at every VAD
    /// finalize to attribute utterances on a shared / pre-mixed vox
    /// channel to the person actually talking. Kept behind its own
    /// mutex — hints arrive on the HTTP thread pool, VAD finalizes on
    /// tokio worker tasks; both paths are short so contention is
    /// negligible.
    hints: Arc<Mutex<VecDeque<SpeakerHint>>>,
    /// Fan-out for the OBS subtitles overlay: paragraph/clip changes
    /// (only while a recording is active) plus explicit recording-active
    /// state transitions so the overlay can swap between the caption
    /// view and the "suspended" placeholder as soon as recording is
    /// started or stopped. Send failures (no subscribers) are ignored —
    /// this is a fire-and-forget channel.
    subtitles: broadcast::Sender<SubtitleEvent>,
    /// Post-mix mic feed. `start_recording` subscribes here to capture
    /// the silence-gated mixed track for the in-flight recording; the
    /// subscription is dropped when the recording finalizes so the RT
    /// producer sees `receiver_count == 0` in idle mode and skips the
    /// per-callback heap allocation that feeds this tap. Held on
    /// `RecordState` so per-recording worker spawns can pick it up
    /// without extra plumbing through the HTTP handlers.
    monitor_tap: broadcast::Sender<Arc<[f32]>>,
    /// Offline SenseVoice handle. Held here so pass-3 boundary
    /// re-transcription in [`Self::schedule_boundary_retranscribe`] can
    /// call `stt.transcribe` from a spawned task without threading the
    /// handle through the record HTTP surface. `None` when STT was
    /// disabled at startup — pass 3 short-circuits.
    stt: Option<SttHandle>,
    /// Pass-4 LLM scheduler. Set post-construction by main.rs after
    /// both `RecordState` and the chat client are available. `finalize_
    /// clip` fans out wake / forced-break triggers here so the per-
    /// channel LLM task can consider re-organizing the hot zone.
    /// `OnceLock` (not `Mutex<Option<..>>`) because the set is a
    /// one-shot at startup — no need to re-wire midflight.
    llm_scheduler: Arc<OnceLock<LlmScheduler>>,
    /// Runtime toggle: when `false` (the default), pass-4 LLM cycles
    /// are skipped while no named recording is active — the live
    /// buffer stays as a plain per-clip transcript. Flipping to `true`
    /// lets the LLM reorganize live-mode paragraphs too. Toggled by
    /// the UI via `PUT /record/llm-when-idle`; the Svelte page
    /// mirrors the value in localStorage and posts it on every
    /// change / on mount, so the server just reflects the last-writer.
    llm_when_idle: Arc<AtomicBool>,
    /// Currently-active server-driven playback of a saved recording (or
    /// segment) through the mixer, or `None` when nothing recording-
    /// related is sounding. Written by `begin_playback` /
    /// `end_playback`; read by `playback_snapshot` to synthesize a
    /// live-position field for `GET /record`. Held behind its own mutex
    /// (rather than folded into `Inner`) so a user hammering the Play
    /// button doesn't lock the transcription hot path even briefly.
    playback: Arc<Mutex<Option<RecordPlayback>>>,
}

/// Playback mode for a server-driven recording playback. Master plays
/// the full audio.wav from a start offset; Clip plays a single segment
/// starting at zero. Kept explicit rather than folded into `clip_id`
/// so the client can style the two affordances differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordPlaybackMode {
    Master,
    Clip,
}

impl RecordPlaybackMode {
    #[inline]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Master => "master",
            Self::Clip => "clip",
        }
    }
}

/// Live state for one server-driven recording playback. Position is
/// derived by subtracting `burst_start_frames` from the mixer's running
/// pop counter — the mixer only knows "N frames of the current PCM
/// burst have played", so pinning which recording that burst belongs
/// to has to live here.
#[derive(Debug, Clone)]
pub struct RecordPlayback {
    pub recording_id: String,
    pub clip_id: Option<String>,
    pub mode: RecordPlaybackMode,
    /// PlayPcm sequence claimed at burst dispatch. If the mixer's
    /// `latest_play_seq()` diverges, some newer playback has taken
    /// over — the position we'd report would be for a different burst,
    /// so treat it as inactive.
    pub play_seq: u64,
    /// Value of `mixer.tts_frames_played()` at burst start. All
    /// positions are computed as `current - burst_start_frames`.
    pub burst_start_frames: u64,
    /// Total stereo frames in this burst — used both to cap the
    /// reported position and to compute a duration_ms for the client.
    pub total_frames: u64,
    pub sample_rate: u32,
    /// For master playback, the audio-timeline offset the burst starts
    /// at (i.e. what the client asked for as `startMs`). Position
    /// reported to the client is offset by this so it lines up with
    /// the recording's own audio timeline. Clip playback anchors at 0.
    pub start_offset_ms: u64,
}

/// Serialized playback state for `GET /record`. `None` when nothing is
/// currently playing. `sample_time_ms` is a wall-clock stamp of when
/// the snapshot was measured so the client can interpolate forward
/// smoothly between polls without needing per-frame updates from the
/// server.
#[derive(Debug, Clone, Serialize)]
pub struct RecordPlaybackSnapshot {
    pub recording_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clip_id: Option<String>,
    pub mode: &'static str,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub sample_time_ms: u64,
}

/// Events fanned out to OBS subtitle overlay subscribers.
///
/// * `ParagraphUpsert` — a paragraph appeared or its text/clip list
///   changed. Overlay subscribers key by paragraph id.
/// * `ParagraphRemove` — paragraph deleted (e.g. all clips filtered out
///   as false positives).
/// * `ClipUpsert` — one clip inside a paragraph changed (streaming
///   partial or finalized). Carries the parent paragraph id so overlays
///   that render per-clip can update in place.
/// * `ClipRemove` — clip gone (removed after a false-positive filter).
/// * `RecordingActive` — swap between the caption view and the
///   "suspended" placeholder without waiting for a poll.
#[derive(Debug, Clone)]
pub enum SubtitleEvent {
    ParagraphUpsert(Paragraph),
    ParagraphRemove(String),
    ClipUpsert {
        paragraph_id: String,
        clip: ClipRef,
    },
    ClipRemove {
        paragraph_id: String,
        clip_id: String,
    },
    RecordingActive(bool),
}

impl RecordState {
    pub fn new(
        sample_rate: u32,
        mixer: Arc<AtomicMixer>,
        monitor_tap: broadcast::Sender<Arc<[f32]>>,
        stt: Option<SttHandle>,
    ) -> Self {
        let (subtitles, _) = broadcast::channel::<SubtitleEvent>(64);
        // Initialize one ChannelState per configured Vox slot plus a
        // trailing TTS pseudo-channel. The vector's shape is stable for
        // the process's lifetime so downstream code can rely on indexing
        // by slot.
        let slot_count = mixer.vox_slot_count();
        let mut channels = Vec::with_capacity(slot_count + 1);
        for slot in 0..slot_count {
            let name = mixer
                .vox_slot(slot)
                .map(|s| s.name())
                .unwrap_or_else(|| format!("Vox {}", slot + 1));
            channels.push(ChannelState {
                kind: ChannelKind::Vox(slot),
                channel_name: name,
                paragraphs: Vec::new(),
                audio_ring: SlotAudioRing::new(sample_rate),
                llm_inflight: false,
            });
        }
        // TTS channel ring is unused (TTS clips never feed audio) but we
        // still allocate it with the same rate for shape consistency.
        channels.push(ChannelState {
            kind: ChannelKind::Tts,
            channel_name: TTS_CHANNEL_NAME.to_string(),
            paragraphs: Vec::new(),
            audio_ring: SlotAudioRing::new(sample_rate),
            llm_inflight: false,
        });
        Self {
            inner: Arc::new(Mutex::new(Inner {
                channels,
                active: None,
                boundary_last_fire_ms: HashMap::new(),
            })),
            sample_rate,
            mixer,
            hints: Arc::new(Mutex::new(VecDeque::with_capacity(HINT_RING_CAPACITY))),
            subtitles,
            monitor_tap,
            stt,
            llm_scheduler: Arc::new(OnceLock::new()),
            llm_when_idle: Arc::new(AtomicBool::new(false)),
            playback: Arc::new(Mutex::new(None)),
        }
    }

    /// Register a new server-driven playback burst. Called by the HTTP
    /// handler *after* it has already bumped `tts_stop_gen` (preempting
    /// any previous burst), claimed a fresh `play_seq`, and snapshotted
    /// `mixer.tts_frames_played()` — so the exact atomic-ordering is
    /// visible at the call site, not obscured behind this helper.
    pub fn begin_playback(&self, p: RecordPlayback) {
        let mut guard = self.playback.lock().expect("playback mutex poisoned");
        *guard = Some(p);
    }

    /// Drop the current playback tracker. Called both when the user
    /// explicitly stops and when the snapshot function decides the
    /// burst is over (seq mismatch or position past end).
    pub fn end_playback(&self) {
        let mut guard = self.playback.lock().expect("playback mutex poisoned");
        *guard = None;
    }

    /// Compute a snapshot of the current playback, if any. Auto-clears
    /// the tracker when the burst has completed (position >= duration)
    /// or been superseded (play_seq mismatch) so the client eventually
    /// sees `playback: null` without a separate reaper task.
    pub fn playback_snapshot(&self) -> Option<RecordPlaybackSnapshot> {
        let mut guard = self.playback.lock().expect("playback mutex poisoned");
        let Some(p) = guard.as_ref() else {
            return None;
        };
        let latest_seq = self.mixer.latest_play_seq();
        if p.play_seq != latest_seq {
            *guard = None;
            return None;
        }
        let played = self
            .mixer
            .tts_frames_played()
            .saturating_sub(p.burst_start_frames)
            .min(p.total_frames);
        let sample_rate = p.sample_rate.max(1) as u64;
        let duration_ms = p.total_frames * 1000 / sample_rate;
        let position_ms = p.start_offset_ms + played * 1000 / sample_rate;
        let done = played >= p.total_frames;
        let snap = RecordPlaybackSnapshot {
            recording_id: p.recording_id.clone(),
            clip_id: p.clip_id.clone(),
            mode: p.mode.as_str(),
            position_ms,
            duration_ms: p.start_offset_ms + duration_ms,
            sample_time_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        };
        if done {
            *guard = None;
        }
        Some(snap)
    }

    /// Whether the pass-4 LLM should run when no named recording is
    /// active. Read by `paragraph::run_once` at the top of each cycle.
    pub fn llm_when_idle(&self) -> bool {
        self.llm_when_idle.load(Ordering::Relaxed)
    }

    /// Set the "reinterpret with LLM (live)" runtime toggle. Called by
    /// the HTTP layer on every UI change; the frontend persists the
    /// choice in localStorage so the server just mirrors last-writer.
    pub fn set_llm_when_idle(&self, enabled: bool) {
        self.llm_when_idle.store(enabled, Ordering::Relaxed);
    }

    /// Register the pass-4 LLM scheduler. Called once by main.rs after
    /// the chat client + RecordState are both available. Silently no-ops
    /// on repeated calls (OnceLock semantics).
    pub fn set_llm_scheduler(&self, sched: LlmScheduler) {
        let _ = self.llm_scheduler.set(sched);
    }

    /// Fire a `Wake` trigger at the LLM scheduler for the given channel.
    /// No-op when the scheduler wasn't registered (chat client / STT
    /// disabled at startup) or for the TTS pseudo-channel (never
    /// re-organized).
    pub(crate) fn wake_llm(&self, kind: ChannelKind) {
        let Some(sched) = self.llm_scheduler.get() else {
            return;
        };
        if let ChannelKind::Vox(slot) = kind {
            sched.wake(slot);
        }
    }

    /// Fire a `ForcedBreak` trigger — the previous paragraph on this
    /// channel just closed due to a > PARAGRAPH_GAP_MS silence gap, so
    /// the LLM should re-organize soon regardless of debounce.
    pub(crate) fn forced_break(&self, kind: ChannelKind) {
        let Some(sched) = self.llm_scheduler.get() else {
            return;
        };
        if let ChannelKind::Vox(slot) = kind {
            sched.forced_break(slot);
        }
    }

    /// Flip the transient `llm_inflight` flag on a channel. Called by
    /// the LLM scheduler task around the actual chat completion call so
    /// the snapshot exposes an accurate "reorganizing…" indicator to
    /// the UI. Also broadcasts a per-paragraph `ParagraphUpsert` for
    /// every non-hardened paragraph on the channel while a recording is
    /// active, so SSE overlays without polling still see the state
    /// change ripple.
    pub(crate) fn set_llm_inflight(&self, slot: usize, inflight: bool) {
        let mut g = self.inner.lock().expect("record state mutex poisoned");
        let Some(ch) = g.channels.iter_mut().find(|c| c.kind == ChannelKind::Vox(slot)) else {
            return;
        };
        if ch.llm_inflight == inflight {
            return;
        }
        ch.llm_inflight = inflight;
        // No SSE fanout here — the UI polls /record every 500 ms and
        // channel.llm_inflight is exposed in `ChannelParagraphs`.
        // Overlays that need push-based updates can add a channel-level
        // event later; the LLM cycle is short enough (seconds) that
        // polling already gives good responsiveness.
    }

    /// Snapshot the paragraph list for a given Vox slot. Used by
    /// `paragraph::LlmScheduler` under a short lock; returned data is
    /// owned so the LLM decode step never holds the state mutex.
    pub(crate) fn snapshot_channel_paragraphs(
        &self,
        slot: usize,
    ) -> Option<Vec<Paragraph>> {
        let g = self.inner.lock().expect("record state mutex poisoned");
        g.channels
            .iter()
            .find(|c| c.kind == ChannelKind::Vox(slot))
            .map(|c| c.paragraphs.clone())
    }

    /// Subscribe to the OBS subtitles fan-out. Yields paragraph/clip
    /// changes (only while a named recording is in flight) plus
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
    /// Returns `None` when no hint is applicable — the paragraph then
    /// just carries its channel tag without a speaker label.
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
    /// `"Vox <slot+1>"` for out-of-range indices so paragraph entries
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
        let now_ms = unix_now_ms();
        // Pass 4 runs during any named recording, and in live-buffer
        // mode only when the operator opted in via "Reinterpret with
        // LLM". When pass 4 is going to fire, hardening must wait
        // until every non-provisional clip has been polished — the UI
        // shouldn't flip to the grey "hardened" indicator while the
        // LLM edits are still landing. Read once here so the closure
        // below doesn't re-check per paragraph. Also short-circuits
        // to false when the compile-time master switch is off so
        // hardening reverts to the plain 2 s timeout.
        let pass4_will_run = crate::paragraph::PASS4_ENABLED
            && (g.active.is_some() || self.llm_when_idle());
        // Timeout-based hardening: once a paragraph's last clip ended
        // more than `PARAGRAPH_GAP_MS` ago (same threshold that would
        // open a new paragraph on the next clip anyway), it can no
        // longer grow. Flip `hardened=true` in the serialized snapshot
        // so the UI's state dot goes grey without waiting for a new
        // clip to arrive and trigger the imperative harden path.
        // Snapshot-only — doesn't mutate stored state.
        //
        // Also derive `p.pass4_ran` from clip-level state so the UI
        // can distinguish "condensed" from "reorganized" — pass 4 is
        // per-clip now, so the paragraph is "reorganized" iff every
        // non-provisional clip has been through pass 4.
        let harden_by_timeout = |p: &mut Paragraph| {
            let non_provisional: Vec<&ClipRef> =
                p.clips.iter().filter(|c| !c.provisional).collect();
            let all_pass4 = !non_provisional.is_empty()
                && non_provisional.iter().all(|c| c.pass4_ran);
            p.pass4_ran = all_pass4;
            if p.hardened {
                return;
            }
            let Some(last) = p.clips.last() else { return };
            let last_end = last
                .audio_duration_ms
                .map(|d| last.start_wall_ms.saturating_add(d))
                .unwrap_or(last.start_wall_ms);
            let elapsed = now_ms.saturating_sub(last_end);
            if elapsed < PARAGRAPH_GAP_MS {
                return;
            }
            // Defer hardening while pass 4 is expected but hasn't
            // finished polishing every non-provisional clip yet.
            // Hard-cap the wait at `HARDEN_MAX_WAIT_MS` so a broken
            // LLM doesn't leave paragraphs perpetually un-hardened —
            // after that ceiling we mark hardened anyway and the UI
            // reflects the raw text.
            let awaiting_pass4 = pass4_will_run
                && non_provisional.iter().any(|c| !c.pass4_ran);
            if awaiting_pass4 && elapsed < HARDEN_MAX_WAIT_MS {
                return;
            }
            p.hardened = true;
        };
        let active_recording = g.active.as_ref().map(|a| {
            // Project each paragraph with freshly-computed
            // `mixed_start_ms` values so the client's mixed-audio
            // playhead can highlight the right clip even mid-recording,
            // before any of this is stamped into the persisted paragraph
            // JSON at stop time.
            let paragraphs: Vec<Paragraph> = a
                .paragraphs
                .iter()
                .map(|p| {
                    let mut out = p.clone();
                    for clip in out.clips.iter_mut() {
                        clip.mixed_start_ms = a.wall_to_mixed_ms(clip.start_wall_ms);
                    }
                    harden_by_timeout(&mut out);
                    out
                })
                .collect();
            ActiveRecordingSnapshot {
                id: a.id.clone(),
                name: a.name.clone(),
                created_at: a.created_at,
                paragraphs,
                duration_ms: audio_duration_ms(a.audio.len(), a.sample_rate),
                mixed_duration_ms: audio_duration_ms(a.mixed_audio.len(), a.sample_rate),
            }
        });
        // Live channel-name read from the mixer for the master list.
        let channel_names: Vec<String> = (0..self.mixer.vox_slot_count())
            .map(|i| self.channel_name(i))
            .collect();
        // Build per-channel paragraph lists in the same order as
        // `channels` — vox slots first, TTS last.
        let paragraphs_by_channel: Vec<ChannelParagraphs> = g
            .channels
            .iter()
            .map(|c| {
                // Use the live channel name for the wrapper (so a
                // rename shows up), but leave the paragraphs' embedded
                // channel field pinned to what was captured at push time.
                let channel_name = match c.kind {
                    ChannelKind::Vox(slot) => self.channel_name(slot),
                    ChannelKind::Tts => TTS_CHANNEL_NAME.to_string(),
                };
                let paragraphs: Vec<Paragraph> = c
                    .paragraphs
                    .iter()
                    .map(|p| {
                        let mut out = p.clone();
                        harden_by_timeout(&mut out);
                        out
                    })
                    .collect();
                ChannelParagraphs {
                    channel: channel_name,
                    paragraphs,
                    llm_inflight: c.llm_inflight,
                }
            })
            .collect();
        RecordStateSnapshot {
            mode,
            paragraphs_by_channel,
            active_recording,
            channel_names,
            llm_when_idle: self.llm_when_idle(),
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
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
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
            paragraphs: Vec::new(),
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
        // Recording started → clear the ephemeral live-pane paragraphs
        // so the operator has a clean slate. Anything spoken from now
        // on lands in the fresh active bucket and the (now-empty)
        // channel logs in lockstep.
        for ch in g.channels.iter_mut() {
            ch.paragraphs.clear();
        }
        drop(g);
        let _ = self.subtitles.send(SubtitleEvent::RecordingActive(true));
        Ok(id)
    }

    /// Take the in-flight recording out of state. Caller (the /stop handler)
    /// then encodes both WAVs and hands the paragraph JSON to the store.
    /// Returns `None` when nothing is active OR when the id doesn't match
    /// the current bucket — race-safe against a double-stop.
    ///
    /// Before draining, close any still-open speech window in the mixed
    /// map and stamp each clip with its final `mixed_start_ms` so the
    /// persisted paragraph JSON can drive the mixed-audio playhead
    /// without needing to re-derive the map at read time.
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
        // extrapolation. Without this, the last clip recorded right
        // before stop would land at wall_ms > wall_end_ms and get pinned
        // to the window's end instead of its real position.
        if let Some(last) = active.mixed_map.last_mut() {
            if last.wall_end_ms == u64::MAX {
                last.wall_end_ms = unix_now_ms();
            }
        }
        // Stamp each clip's mixed_start_ms in place before draining.
        // We snapshot the map first so the &active borrow releases
        // before iterating paragraphs mutably.
        let stamps: Vec<Vec<Option<u64>>> = active
            .paragraphs
            .iter()
            .map(|p| {
                p.clips
                    .iter()
                    .map(|c| active.wall_to_mixed_ms(c.start_wall_ms))
                    .collect()
            })
            .collect();
        for (paragraph, clip_stamps) in active.paragraphs.iter_mut().zip(stamps) {
            for (clip, mixed) in paragraph.clips.iter_mut().zip(clip_stamps) {
                clip.mixed_start_ms = mixed;
            }
        }
        // Recording finished → clear the ephemeral live-pane buffer.
        // The saved recording carries its own paragraph list to disk;
        // the per-channel logs would otherwise linger showing stale
        // paragraphs across sessions. Deliberately after we've drained
        // `active` so the returned `TakenRecording` is intact.
        for ch in g.channels.iter_mut() {
            ch.paragraphs.clear();
        }
        Some(TakenRecording {
            id: active.id,
            name: active.name,
            created_at: active.created_at,
            paragraphs: active.paragraphs,
            audio: active.audio,
            mixed_audio: active.mixed_audio,
            sample_rate: active.sample_rate,
        })
    }

    /// Empty every channel's paragraph log. Does NOT touch the in-flight
    /// active recording — clearing what the user is looking at on the
    /// Live pane must never wipe a session that's still capturing.
    pub fn clear_channels(&self) {
        let mut g = self.inner.lock().expect("record state mutex poisoned");
        for ch in g.channels.iter_mut() {
            ch.paragraphs.clear();
        }
    }

    /// Update the text on a clip that lives anywhere in the channel
    /// paragraph logs or the currently-active recording. Returns `true`
    /// when a matching clip was found and mutated. Empty / whitespace-
    /// only text is rejected — the caller preserves the original rather
    /// than silently blanking a clip.
    ///
    /// Rebuilds the containing paragraph's `text`/`raw_text` after the
    /// edit and broadcasts a [`SubtitleEvent::ClipUpsert`] so overlay
    /// subscribers pick up the change.
    pub fn update_clip_text(&self, clip_id: &str, text: String) -> bool {
        let text = text.trim().to_string();
        if text.is_empty() {
            return false;
        }
        let mut g = self.inner.lock().expect("record state mutex poisoned");
        let mut updated_pair: Option<(String, ClipRef)> = None;
        for ch in g.channels.iter_mut() {
            let hit = update_clip_in_paragraphs(&mut ch.paragraphs, clip_id, &text);
            if let Some((pid, clip)) = hit {
                updated_pair = Some((pid, clip));
                break;
            }
        }
        // Mirror the same edit into the active recording's paragraph list.
        if let Some(active) = g.active.as_mut() {
            if let Some(pair) = update_clip_in_paragraphs(&mut active.paragraphs, clip_id, &text) {
                if updated_pair.is_none() {
                    updated_pair = Some(pair);
                }
            }
        }
        let recording = g.active.is_some();
        drop(g);
        if let Some((paragraph_id, clip)) = updated_pair {
            if recording {
                let _ = self
                    .subtitles
                    .send(SubtitleEvent::ClipUpsert { paragraph_id, clip });
            }
            true
        } else {
            false
        }
    }

    /// Pass-4 completion path: replace `clip_id`'s text with the LLM's
    /// polished version and flip `pass4_ran=true` so future triggers
    /// skip it. Rebuilds the owning paragraph so its aggregated
    /// `raw_text` / `text` reflect the new clip content, then emits a
    /// `ClipUpsert` (only if a named recording is active — matches the
    /// finalize/pass-3 pattern).
    ///
    /// Returns `true` when a matching clip was found and updated. A
    /// paragraph reorg between snapshot and apply (or the clip being
    /// removed as noise) is a benign no-op.
    pub(crate) fn apply_pass4_clip_text(&self, clip_id: &str, text: String) -> bool {
        let text = text.trim().to_string();
        if text.is_empty() {
            return false;
        }
        let mut g = self.inner.lock().expect("record state mutex poisoned");
        let mut updated_pair: Option<(String, ClipRef)> = None;
        for ch in g.channels.iter_mut() {
            for paragraph in ch.paragraphs.iter_mut() {
                if let Some(clip) = paragraph.clips.iter_mut().find(|c| c.id == clip_id) {
                    clip.text = text.clone();
                    clip.pass4_ran = true;
                    let snap = clip.clone();
                    paragraph.rebuild();
                    updated_pair = Some((paragraph.id.clone(), snap));
                    break;
                }
            }
            if updated_pair.is_some() {
                break;
            }
        }
        if let Some(active) = g.active.as_mut() {
            for paragraph in active.paragraphs.iter_mut() {
                if let Some(clip) = paragraph.clips.iter_mut().find(|c| c.id == clip_id) {
                    clip.text = text.clone();
                    clip.pass4_ran = true;
                    paragraph.rebuild();
                    break;
                }
            }
        }
        let recording = g.active.is_some();
        drop(g);
        if let Some((paragraph_id, clip)) = updated_pair {
            if recording {
                let _ = self
                    .subtitles
                    .send(SubtitleEvent::ClipUpsert { paragraph_id, clip });
            }
            true
        } else {
            false
        }
    }

    /// Append a user-authored correction patch to a paragraph. Mirrors
    /// the edit onto both the channel-log copy and the active-recording
    /// copy so the two views stay in lockstep. Returns the newly-minted
    /// [`ParagraphEdit`] (with its assigned id and timestamp) when a
    /// paragraph with the given id was found; `None` when the paragraph
    /// belongs to a saved recording (caller should use the sqlite
    /// path).
    pub fn add_paragraph_edit(
        &self,
        paragraph_id: &str,
        original: String,
        replacement: String,
    ) -> Option<ParagraphEdit> {
        let edit = ParagraphEdit {
            id: Uuid::new_v4().to_string(),
            original,
            replacement,
            created_at: unix_now(),
        };
        let mut g = self.inner.lock().expect("record state mutex poisoned");
        let mut hit = false;
        for ch in g.channels.iter_mut() {
            for p in ch.paragraphs.iter_mut() {
                if p.id == paragraph_id {
                    p.edits.push(edit.clone());
                    hit = true;
                    break;
                }
            }
            if hit { break; }
        }
        if let Some(active) = g.active.as_mut() {
            for p in active.paragraphs.iter_mut() {
                if p.id == paragraph_id {
                    p.edits.push(edit.clone());
                    hit = true;
                    break;
                }
            }
        }
        if hit { Some(edit) } else { None }
    }

    /// Soft-delete a paragraph — flip its `deleted` flag in both the
    /// per-channel log and the active recording (if present). Returns
    /// `true` when a matching paragraph was found. Existing edits and
    /// clip references are preserved so an undelete path is a
    /// straightforward flip.
    pub fn delete_paragraph(&self, paragraph_id: &str) -> bool {
        let mut g = self.inner.lock().expect("record state mutex poisoned");
        let mut hit = false;
        for ch in g.channels.iter_mut() {
            for p in ch.paragraphs.iter_mut() {
                if p.id == paragraph_id {
                    p.deleted = true;
                    hit = true;
                    break;
                }
            }
        }
        if let Some(active) = g.active.as_mut() {
            for p in active.paragraphs.iter_mut() {
                if p.id == paragraph_id {
                    p.deleted = true;
                    hit = true;
                    break;
                }
            }
        }
        hit
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

    /// Pass 1 upsert: a streaming partial (or an initial insertion) for
    /// `clip_id` on the given `slot`. Opens a new paragraph if the last
    /// clip on this channel ended more than [`PARAGRAPH_GAP_MS`] before
    /// `start_wall_ms` — otherwise appends (or updates in place) inside
    /// the current growing paragraph. Broadcasts [`SubtitleEvent::
    /// ClipUpsert`] (and [`SubtitleEvent::ParagraphUpsert`] on the paragraph
    /// creation path) so subscribers see mid-utterance updates.
    ///
    /// No false-positive filter here — partials should show the live
    /// hypothesis even if it briefly matches a filter phrase; the filter
    /// runs only at [`Self::finalize_clip`] time when the final text is
    /// what we're deciding about.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_provisional_clip(
        &self,
        slot: usize,
        clip_id: &str,
        text: String,
        audio_start_ms: Option<u64>,
        audio_duration_ms: Option<u64>,
        speaker: Option<String>,
        start_wall_ms: u64,
    ) {
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        let channel_name = self.channel_name(slot);
        let mut g = self.inner.lock().expect("record state mutex poisoned");
        let (new_paragraph, upserted_clip, paragraph_id) = {
            let Some(ch) = g.channel_mut(ChannelKind::Vox(slot)) else {
                return;
            };
            upsert_clip_into_channel(
                ch,
                clip_id,
                &trimmed,
                audio_start_ms,
                audio_duration_ms,
                &speaker,
                start_wall_ms,
                &channel_name,
                true,
            )
        };
        // Mirror into the active recording if one is running. We look up
        // by paragraph_id — if it wasn't there yet, clone the paragraph
        // over verbatim (single-clip insert); otherwise apply the clip
        // upsert to the existing mirror.
        if let Some(active) = g.active.as_mut() {
            mirror_upsert_into_active(active, &paragraph_id, &upserted_clip, &new_paragraph);
        }
        let recording = g.active.is_some();
        // Live-buffer + LLM-off: no further pass-4 work is planned for
        // the previous paragraph on this channel, so treat the moment
        // it closes (silence >6 s → new paragraph opens) as terminal.
        // Flip it to `hardened` so the UI's state dot goes grey. During
        // a named recording the LLM runs regardless of the toggle;
        // hardening is its job in that mode.
        let harden_prev = new_paragraph.is_some() && !recording && !self.llm_when_idle();
        if harden_prev {
            harden_prev_paragraph(&mut *g, ChannelKind::Vox(slot));
        }
        drop(g);
        if recording {
            if let Some(p) = new_paragraph {
                let _ = self.subtitles.send(SubtitleEvent::ParagraphUpsert(p));
            }
            let _ = self.subtitles.send(SubtitleEvent::ClipUpsert {
                paragraph_id,
                clip: upserted_clip,
            });
        }
    }

    /// Pass 2 finalize: the offline SenseVoice text for `clip_id`. Runs
    /// the same [`is_false_positive`] filter that the legacy
    /// `push_transcript` applied: if the final text matches, the clip is
    /// removed from its paragraph (and the paragraph itself if it now has
    /// zero clips). Otherwise the clip is set to `provisional=false`, the
    /// paragraph is rebuilt, and a `ClipUpsert` fires.
    #[allow(clippy::too_many_arguments)]
    pub fn finalize_clip(
        &self,
        slot: usize,
        clip_id: &str,
        text: String,
        audio_start_ms: Option<u64>,
        audio_duration_ms: Option<u64>,
        speaker: Option<String>,
        start_wall_ms: u64,
    ) {
        let trimmed = text.trim().to_string();
        let is_junk = trimmed.is_empty()
            || !trimmed.chars().any(|c| c.is_alphanumeric())
            || is_false_positive(&trimmed);
        if is_junk {
            // Drop any provisional clip that was already broadcast; keep
            // its paragraph if it still has other surviving clips.
            self.remove_clip(clip_id);
            return;
        }
        let channel_name = self.channel_name(slot);
        let mut g = self.inner.lock().expect("record state mutex poisoned");
        let (new_paragraph, finalized_clip, paragraph_id) = {
            let Some(ch) = g.channel_mut(ChannelKind::Vox(slot)) else {
                return;
            };
            upsert_clip_into_channel(
                ch,
                clip_id,
                &trimmed,
                audio_start_ms,
                audio_duration_ms,
                &speaker,
                start_wall_ms,
                &channel_name,
                false,
            )
        };
        if let Some(active) = g.active.as_mut() {
            mirror_upsert_into_active(active, &paragraph_id, &finalized_clip, &new_paragraph);
        }
        let recording = g.active.is_some();
        // Live-buffer + LLM-off: harden the previous paragraph on this
        // channel now that a new one has opened. Rationale in
        // `upsert_provisional_clip` above. Under the same lock so the
        // UI's next poll observes a coherent state.
        let harden_prev = new_paragraph.is_some() && !recording && !self.llm_when_idle();
        if harden_prev {
            harden_prev_paragraph(&mut *g, ChannelKind::Vox(slot));
        }
        // Second, independent soft-cap trigger: VAD silence-close (a
        // real user pause, not a max-length rollover) at word count ≥
        // `PARAGRAPH_SOFT_MAX_WORDS`. This fires regardless of whether
        // pass 3 has run. In non-stop-speech mode the pass-3 window
        // often exceeds the audio-span budget and skips — leaving the
        // pass-3-based soft-cap dormant. This one uses a signal that
        // doesn't need pass 3: the clip's own duration. Anything
        // strictly under `VAD_MAX_UTTERANCE_MS` came from VAD's
        // silence-end trigger (user paused ≥ VAD_SILENCE_END_MS),
        // which is a natural sentence boundary in practice. Skips
        // LLM-authored paragraphs (`pass4_ran`) — LLM owns structural
        // breaks there. Only meaningful for the paragraph the current
        // clip just landed in; that's `paragraph_id`.
        let silence_closed = audio_duration_ms
            .map(|d| d < VAD_MAX_UTTERANCE_MS as u64)
            .unwrap_or(true);
        // Two-tier soft-cap: at the soft threshold, only close on a
        // natural boundary (silence-close). At the hard threshold,
        // force-close regardless — this handles non-stop-speech where
        // no silence-close ever fires (VAD keeps hitting max-length
        // rollovers with no perceptible pauses).
        let mark_paragraph_closed = |g_inner: &mut Inner, log_reason: &str| {
            if let Some(ch) = g_inner
                .channels
                .iter_mut()
                .find(|c| c.kind == ChannelKind::Vox(slot))
            {
                if let Some(p) = ch.paragraphs.iter_mut().find(|p| p.id == paragraph_id) {
                    if !p.pass4_ran && !p.closed {
                        p.closed = true;
                        debug!(
                            paragraph = %paragraph_id,
                            words = count_words(&p.text),
                            reason = log_reason,
                            "soft-cap: paragraph closed"
                        );
                    }
                }
            }
            if let Some(active) = g_inner.active.as_mut() {
                if let Some(p) = active.paragraphs.iter_mut().find(|p| p.id == paragraph_id) {
                    if !p.pass4_ran && !p.closed {
                        p.closed = true;
                    }
                }
            }
        };
        let cur_words = g
            .channels
            .iter()
            .find(|c| c.kind == ChannelKind::Vox(slot))
            .and_then(|c| c.paragraphs.iter().find(|p| p.id == paragraph_id))
            .map(|p| count_words(&p.text))
            .unwrap_or(0);
        if cur_words >= PARAGRAPH_HARD_MAX_WORDS {
            mark_paragraph_closed(&mut *g, "hard-cap");
        } else if silence_closed && cur_words >= PARAGRAPH_SOFT_MAX_WORDS {
            mark_paragraph_closed(&mut *g, "silence-close");
        }
        drop(g);
        // Snapshot whether a fresh paragraph was opened (which happens
        // only when the prior paragraph's last clip was > PARAGRAPH_GAP_MS
        // ago on this channel — i.e. a silence-closed boundary). We use
        // this AFTER dropping the lock to fire a ForcedBreak trigger.
        let forced_break_needed = new_paragraph.is_some();
        if recording {
            if let Some(p) = new_paragraph {
                let _ = self.subtitles.send(SubtitleEvent::ParagraphUpsert(p));
            }
            let _ = self.subtitles.send(SubtitleEvent::ClipUpsert {
                paragraph_id: paragraph_id.clone(),
                clip: finalized_clip,
            });
        }
        // Pass 3 — boundary re-transcription. Fires unconditionally on
        // every clip finalize; the schedule fn itself handles
        // debouncing, TTS filtering, and window trimming.
        self.schedule_boundary_retranscribe(paragraph_id);
        // Pass 4 — LLM hot-zone reorganization. Always Wake on finalize;
        // the scheduler task itself decides whether the word/audio
        // thresholds warrant a call. Silence-closed boundaries also fire
        // a ForcedBreak so a queued call goes out soon regardless of the
        // debounce ceiling.
        if forced_break_needed {
            self.forced_break(ChannelKind::Vox(slot));
        }
        self.wake_llm(ChannelKind::Vox(slot));
    }

    /// Remove a clip by id from every channel + the active recording,
    /// and broadcast `ClipRemove` (or `ParagraphRemove` if that clip was
    /// the last one in its paragraph). Used when a streaming partial's
    /// final matched the false-positive filter, or when the operator
    /// deletes a clip explicitly.
    pub fn remove_clip(&self, clip_id: &str) {
        let mut g = self.inner.lock().expect("record state mutex poisoned");
        let mut hit: Option<RemoveOutcome> = None;
        for ch in g.channels.iter_mut() {
            if let Some(outcome) = remove_clip_from_paragraphs(&mut ch.paragraphs, clip_id) {
                hit = Some(outcome);
                break;
            }
        }
        // Mirror the same removal into the active recording's paragraphs.
        // We take the outcome from the channel side if we had one;
        // otherwise fall back to whatever the active recording produces.
        let active_outcome = g
            .active
            .as_mut()
            .and_then(|a| remove_clip_from_paragraphs(&mut a.paragraphs, clip_id));
        if hit.is_none() {
            hit = active_outcome;
        }
        let recording = g.active.is_some();
        drop(g);
        let Some(outcome) = hit else {
            return;
        };
        if !recording {
            return;
        }
        match outcome {
            RemoveOutcome::ClipGone {
                paragraph_id,
                clip_id,
            } => {
                let _ = self.subtitles.send(SubtitleEvent::ClipRemove {
                    paragraph_id,
                    clip_id,
                });
            }
            RemoveOutcome::ParagraphGone(pid) => {
                let _ = self.subtitles.send(SubtitleEvent::ParagraphRemove(pid));
            }
        }
    }

    /// Log a TTS playback as a single-clip paragraph on the TTS
    /// pseudo-channel. `hardened=true` immediately — TTS text is already
    /// LLM-authored and never re-transcribed in later stages.
    ///
    /// Always appends to the TTS channel's paragraph log so the Live
    /// view shows every TTS clip the user hears — including clips
    /// played with no recording active.
    pub fn push_tts_paragraph(
        &self,
        text: String,
        audio_url: Option<String>,
        voice_label: Option<String>,
    ) {
        let clip = ClipRef {
            id: Uuid::new_v4().to_string(),
            audio_start_ms: None,
            audio_duration_ms: None,
            start_wall_ms: unix_now_ms(),
            text: text.clone(),
            provisional: false,
            audio_url,
            mixed_start_ms: None,
            // TTS text is authoritative — no re-transcription happens,
            // but the punctuation is already real. Mark pass3_ran=true
            // so the paragraph soft-cap treats TTS trailing punctuation
            // as reliable. TTS text is also LLM-authored (from the
            // /say or /chat call), so pass 4 has nothing to correct —
            // mark pass4_ran=true to skip it.
            pass3_ran: true,
            pass4_ran: true,
        };
        let speaker = voice_label.and_then(|s| {
            let t = s.trim().to_string();
            (!t.is_empty()).then_some(t)
        });
        let paragraph = Paragraph {
            id: Uuid::new_v4().to_string(),
            channel: TTS_CHANNEL_NAME.to_string(),
            speaker,
            hardened: true,
            text: text.clone(),
            raw_text: text,
            clips: vec![clip],
            start_wall_ms: unix_now_ms(),
            end_wall_ms: unix_now_ms(),
            created_at: unix_now(),
            // TTS paragraphs are authoritative on arrival: no
            // re-transcription happens, and the LLM never touches them.
            // Treat both as "already run" so the UI shows them as
            // fully-processed hardened blocks straight away.
            pass3_ran: true,
            pass4_ran: true,
            pass3_inflight: false,
            closed: false,
            edits: Vec::new(),
            deleted: false,
        };
        let mut g = self.inner.lock().expect("record state mutex poisoned");
        if let Some(ch) = g.channel_mut(ChannelKind::Tts) {
            ch.paragraphs.push(paragraph.clone());
            trim_channel_cap(ch);
        }
        // Mirror into the active recording if one is running.
        if let Some(active) = g.active.as_mut() {
            active.paragraphs.push(paragraph.clone());
        }
        let recording = g.active.is_some();
        drop(g);
        if recording {
            let _ = self.subtitles.send(SubtitleEvent::ParagraphUpsert(paragraph));
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

    /// Downmix and push a stereo interleaved chunk into the given vox
    /// slot's audio ring. Called from `spawn_worker`'s recv loop on every
    /// incoming chunk (regardless of the slot's enable flag) so pass-3
    /// boundary re-transcription has audio available even when the
    /// operator toggles enable mid-utterance or nothing is recording.
    fn push_slot_audio(&self, slot: usize, stereo_chunk: &[f32]) {
        if stereo_chunk.is_empty() {
            return;
        }
        // Downmix to mono up front so we don't hold the lock across the
        // allocation.
        let mut mono = Vec::with_capacity(stereo_chunk.len() / 2);
        for pair in stereo_chunk.chunks_exact(2) {
            mono.push((pair[0] + pair[1]) * 0.5);
        }
        let now_ms = unix_now_ms();
        let mut g = self.inner.lock().expect("record state mutex poisoned");
        if let Some(ch) = g.channel_mut(ChannelKind::Vox(slot)) {
            ch.audio_ring.push(&mono, now_ms);
        }
    }

    /// Mark the given vox slot's audio ring as post-lagged. Subsequent
    /// `samples_between` calls return `None` until the next push
    /// re-anchors the ring. Called from `spawn_worker` on a
    /// `broadcast::Lagged` so pass 3 can't quietly hand SenseVoice a
    /// buffer with a hidden gap.
    fn mark_slot_lagged(&self, slot: usize) {
        let mut g = self.inner.lock().expect("record state mutex poisoned");
        if let Some(ch) = g.channel_mut(ChannelKind::Vox(slot)) {
            ch.audio_ring.on_lagged();
        }
    }

    /// Schedule a pass-3 boundary re-transcription for `paragraph_id`.
    /// Runs asynchronously — the caller drops the state lock before
    /// invoking this. Behavior:
    ///
    ///   1. Snapshot the paragraph + owning channel under a short lock.
    ///   2. Skip TTS paragraphs (their text is already LLM-authored).
    ///   3. Skip paragraphs with fewer than 2 finalized clips — nothing
    ///      to consolidate.
    ///   4. Trim the window (last N=3 clips) until total duration ≤
    ///      [`BOUNDARY_MAX_WINDOW_MS`].
    ///   5. Debounce per-paragraph via
    ///      [`Inner::boundary_last_fire_ms`] — skip if the last fire
    ///      was less than [`BOUNDARY_DEBOUNCE_MS`] ago.
    ///   6. Fetch the mono samples from the slot's ring; skip if the
    ///      requested window predates retention or crosses a lag gap.
    ///   7. `spawn_blocking` SenseVoice; on success proportionally split
    ///      the returned text back to the window's clips, snapping cut
    ///      points to nearest whitespace.
    ///   8. Reacquire the lock, re-validate the window (paragraph +
    ///      clip ids still exist, all still `provisional=false`), apply
    ///      the update, rebuild paragraph text, and broadcast
    ///      `ClipUpsert` for each updated clip if a recording is active.
    ///
    /// Never returns errors — logs a `debug` breadcrumb on discard and
    /// a `warn` on STT failure.
    /// Clear the transient pass-3 inflight flag on `paragraph_id` in
    /// both the channel state and the active recording (if mirrored).
    /// Used from early-return branches inside the spawned pass-3 task
    /// so the UI's "condensing…" indicator doesn't get stuck on when
    /// the decode aborts before writing anything.
    fn clear_pass3_inflight(&self, paragraph_id: &str) {
        let mut g = self.inner.lock().expect("record state mutex poisoned");
        for ch in g.channels.iter_mut() {
            if let Some(p) = ch.paragraphs.iter_mut().find(|p| p.id == paragraph_id) {
                p.pass3_inflight = false;
                break;
            }
        }
        if let Some(active) = g.active.as_mut() {
            if let Some(p) = active
                .paragraphs
                .iter_mut()
                .find(|p| p.id == paragraph_id)
            {
                p.pass3_inflight = false;
            }
        }
    }

    fn schedule_boundary_retranscribe(&self, paragraph_id: String) {
        let Some(stt) = self.stt.clone() else {
            // STT disabled at startup — pass 3 is a no-op.
            return;
        };
        // Snapshot window under the lock.
        let sr = self.sample_rate;
        let snapshot = {
            let mut g = self.inner.lock().expect("record state mutex poisoned");
            // Locate the paragraph and its owning channel index.
            let mut hit: Option<(usize, usize)> = None;
            for (ci, ch) in g.channels.iter().enumerate() {
                if let Some(pi) = ch.paragraphs.iter().position(|p| p.id == paragraph_id) {
                    hit = Some((ci, pi));
                    break;
                }
            }
            let Some((ci, pi)) = hit else {
                return; // paragraph vanished (race) — nothing to do
            };
            // TTS paragraphs skip pass 3 entirely.
            let ChannelKind::Vox(slot) = g.channels[ci].kind else {
                return;
            };
            let paragraph = &g.channels[ci].paragraphs[pi];
            // Collect this paragraph's finalized clips with known
            // duration, tagged with the owning paragraph id. Take the
            // most recent up to N=3.
            let mut window: Vec<(String, String, u64, u64, String, bool)> = paragraph
                .clips
                .iter()
                .filter(|c| c.audio_duration_ms.is_some())
                .rev()
                .take(3)
                .map(|c| {
                    (
                        c.id.clone(),
                        paragraph.id.clone(),
                        c.start_wall_ms,
                        c.audio_duration_ms.unwrap_or(0),
                        c.text.clone(),
                        c.provisional,
                    )
                })
                .collect();
            window.reverse();
            // Cross-paragraph borrow: when the current paragraph is
            // short (its first clip just landed), reach back into the
            // PREVIOUS paragraph on the same channel and pull its last
            // finalized clip into the window as CONTEXT audio only.
            // Pass 3 re-transcribes the joined audio; the split is
            // applied ONLY to clips in the current paragraph — the
            // borrowed clip is never written back (see the apply
            // loop). Guards:
            //   (a) current paragraph has fewer than 2 clips (so
            //       pass 3 wouldn't otherwise fire), and
            //   (b) previous paragraph is NOT hardened (either the
            //       flag or the wall-clock timeout past
            //       `PARAGRAPH_GAP_MS` — a hardened paragraph is a
            //       committed boundary and reaching back across it
            //       would defeat the point of hardening), and
            //   (c) the silence gap between the borrow candidate and
            //       the current clip is under
            //       `BOUNDARY_BORROW_MAX_GAP_MS` (a long pause means
            //       the speaker really stopped; no continuous-speech
            //       seam to smooth, and long SenseVoice-across-
            //       silence inputs degrade), and
            //   (d) the resulting WALL-CLOCK audio span fits inside
            //       `BOUNDARY_MAX_WINDOW_MS` (SenseVoice comfort zone).
            // Any failed guard → skip borrow entirely.
            if window.len() < 2 {
                if let Some(prev_idx) = pi.checked_sub(1) {
                    if let Some(prev_p) = g.channels[ci].paragraphs.get(prev_idx) {
                        // Compute effective hardened state (flag OR
                        // timeout past PARAGRAPH_GAP_MS from last
                        // clip's end). The snapshot serializer does
                        // the same computation for UI display; we do
                        // it inline here so behavior matches the
                        // user's mental model regardless of whether
                        // the imperative harden path has fired yet.
                        let prev_last_end = prev_p
                            .clips
                            .last()
                            .map(|c| {
                                c.audio_duration_ms
                                    .map(|d| c.start_wall_ms.saturating_add(d))
                                    .unwrap_or(c.start_wall_ms)
                            })
                            .unwrap_or(0);
                        let now = unix_now_ms();
                        let prev_timed_out =
                            now.saturating_sub(prev_last_end) >= PARAGRAPH_GAP_MS;
                        let prev_effectively_hardened =
                            prev_p.hardened || prev_timed_out;
                        if prev_effectively_hardened {
                            debug!(
                                paragraph = %paragraph_id,
                                prev = %prev_p.id,
                                flag = prev_p.hardened,
                                timed_out = prev_timed_out,
                                "pass3: cross-paragraph borrow declined (prev paragraph hardened)"
                            );
                        } else if let Some(prev_last) = prev_p
                            .clips
                            .iter()
                            .rev()
                            .find(|c| c.audio_duration_ms.is_some() && !c.provisional)
                        {
                            let cur_start = window
                                .last()
                                .map(|(_, _, s, _, _, _)| *s)
                                .unwrap_or(0);
                            let cur_end = window
                                .last()
                                .map(|(_, _, s, d, _, _)| s.saturating_add(*d))
                                .unwrap_or(0);
                            let borrow_end = prev_last
                                .start_wall_ms
                                .saturating_add(prev_last.audio_duration_ms.unwrap_or(0));
                            let gap = cur_start.saturating_sub(borrow_end);
                            let span = cur_end.saturating_sub(prev_last.start_wall_ms);
                            if gap > BOUNDARY_BORROW_MAX_GAP_MS {
                                debug!(
                                    paragraph = %paragraph_id,
                                    gap_ms = gap,
                                    limit_ms = BOUNDARY_BORROW_MAX_GAP_MS,
                                    "pass3: cross-paragraph borrow declined (silence gap too long)"
                                );
                            } else if span > BOUNDARY_MAX_WINDOW_MS {
                                debug!(
                                    paragraph = %paragraph_id,
                                    span_ms = span,
                                    limit_ms = BOUNDARY_MAX_WINDOW_MS,
                                    "pass3: cross-paragraph borrow declined (audio span too long)"
                                );
                            } else {
                                window.insert(
                                    0,
                                    (
                                        prev_last.id.clone(),
                                        prev_p.id.clone(),
                                        prev_last.start_wall_ms,
                                        prev_last.audio_duration_ms.unwrap_or(0),
                                        prev_last.text.clone(),
                                        false,
                                    ),
                                );
                            }
                        }
                    }
                }
            }
            // Still not enough context? Nothing to reconsolidate.
            if window.len() < 2 {
                return;
            }
            // Any provisional in the window? Skip — a fresh pass 2 is
            // still landing, its update will schedule us again.
            if window.iter().any(|(_, _, _, _, _, prov)| *prov) {
                return;
            }
            // Trim from oldest until the WALL-CLOCK audio span fits
            // inside `BOUNDARY_MAX_WINDOW_MS`. Using wall span (not the
            // sum of clip durations) accounts for silence between
            // clips — feeding SenseVoice more than ~25 s of audio can
            // degrade the decode and drop content. Never trim past a
            // 2-clip minimum; if a 2-clip window still exceeds the
            // budget, abort rather than emit a garbage split.
            let window_span = |w: &Vec<(String, String, u64, u64, String, bool)>| -> u64 {
                let Some(first) = w.first() else { return 0 };
                let Some(last) = w.last() else { return 0 };
                last.2.saturating_add(last.3).saturating_sub(first.2)
            };
            while window.len() > 2 && window_span(&window) > BOUNDARY_MAX_WINDOW_MS {
                window.remove(0);
            }
            if window_span(&window) > BOUNDARY_MAX_WINDOW_MS {
                debug!(
                    paragraph = %paragraph_id,
                    span_ms = window_span(&window),
                    limit_ms = BOUNDARY_MAX_WINDOW_MS,
                    "pass3: window audio span exceeds budget after trimming — skipping"
                );
                return;
            }
            let total: u64 = window.iter().map(|(_, _, _, d, _, _)| *d).sum();
            if total == 0 {
                return;
            }
            // Debounce per paragraph.
            let now_ms = unix_now_ms();
            if let Some(last) = g.boundary_last_fire_ms.get(&paragraph_id) {
                if now_ms.saturating_sub(*last) < BOUNDARY_DEBOUNCE_MS {
                    return;
                }
            }
            g.boundary_last_fire_ms.insert(paragraph_id.clone(), now_ms);
            // Compute wall range: [first.start_wall_ms, last.start_wall_ms + last.duration_ms].
            let range_start = window.first().map(|w| w.2).unwrap_or(0);
            let range_end = window
                .last()
                .map(|(_, _, s, d, _, _)| s.saturating_add(*d))
                .unwrap_or(0);
            // Fetch samples now — the ring lives on the same lock, so
            // grabbing them here keeps the async task purely CPU-bound.
            let samples = g.channels[ci]
                .audio_ring
                .samples_between(range_start, range_end);
            // Flip the transient inflight flag on so the UI can surface a
            // "condensing…" pulse. Cleared in every early-return branch
            // below and after the blocking decode returns.
            g.channels[ci].paragraphs[pi].pass3_inflight = true;
            (slot, window, samples)
        };
        let (slot, window, samples) = snapshot;
        let Some(samples) = samples else {
            debug!(
                paragraph = %paragraph_id,
                slot,
                "pass3: audio ring window unavailable — skipping"
            );
            self.clear_pass3_inflight(&paragraph_id);
            return;
        };
        let state = self.clone();
        let paragraph_id_for_task = paragraph_id;
        tokio::spawn(async move {
            let res =
                tokio::task::spawn_blocking(move || stt.transcribe(&samples, sr)).await;
            let text = match res {
                Ok(Ok(t)) => t.trim().to_string(),
                Ok(Err(err)) => {
                    warn!(err = %format!("{err:#}"), "pass3: STT decode failed");
                    state.clear_pass3_inflight(&paragraph_id_for_task);
                    return;
                }
                Err(_) => {
                    warn!("pass3: STT task panicked");
                    state.clear_pass3_inflight(&paragraph_id_for_task);
                    return;
                }
            };
            // SenseVoice-hallucination guards. On short single-word audio,
            // long silences, or otherwise "confusing" inputs, SenseVoice
            // sometimes returns a single false-positive interjection
            // ("Yeah", "Y.", "I.", etc.) or a much shorter transcript than
            // the audio actually contains. Distributing that tiny output
            // across the window's clips WIPES real content ("one, two,
            // three, four, …" → "Yeah" split across N clips = every
            // number gone). Both cases: keep the previous per-clip text
            // and let the next pass 3 fire try again with fresh audio.
            let existing_joined_words: usize = window
                .iter()
                .map(|(_, _, _, _, txt, _)| count_words(txt))
                .sum();
            let out_words = count_words(&text);
            let too_short = existing_joined_words >= 6
                && out_words < (existing_joined_words as f64 * 0.5).round() as usize;
            if is_false_positive(&text) || too_short {
                debug!(
                    paragraph = %paragraph_id_for_task,
                    out_words,
                    existing_joined_words,
                    output = %text,
                    "pass3: SenseVoice output implausibly short — discarding"
                );
                state.clear_pass3_inflight(&paragraph_id_for_task);
                return;
            }
            // Edge-word preservation guard. SenseVoice occasionally
            // drops the very first or last word when re-decoding the
            // concatenated audio (observed: "My grandmother …" losing
            // "My"). A one-word loss out of ~16 is only ~6% of the
            // total, so a bulk word-count check would miss it — but
            // the duration-proportional split shifts every downstream
            // cut, so clip 1's leading word gets replaced and words
            // from clip 2 flow backward into clip 1's row.
            // Cheap precise check: the joined decode's first word must
            // match the first clip's original first word, and its last
            // word must match the last clip's original last word
            // (case- and punctuation-insensitive). Any mismatch means
            // the split would assign shifted content — reject the cycle.
            let normalize_edge = |w: &str| -> String {
                w.trim_matches(|c: char| !c.is_alphanumeric())
                    .to_lowercase()
            };
            let joined_first = text.split_whitespace().next().map(normalize_edge);
            let joined_last = text.split_whitespace().last().map(normalize_edge);
            let first_clip_first = window
                .first()
                .and_then(|(_, _, _, _, t, _)| t.split_whitespace().next().map(normalize_edge));
            let last_clip_last = window
                .last()
                .and_then(|(_, _, _, _, t, _)| t.split_whitespace().last().map(normalize_edge));
            if joined_first != first_clip_first || joined_last != last_clip_last {
                info!(
                    paragraph = %paragraph_id_for_task,
                    joined_first = ?joined_first,
                    first_clip_first = ?first_clip_first,
                    joined_last = ?joined_last,
                    last_clip_last = ?last_clip_last,
                    output = %text,
                    "pass3-diag: edge word mismatch — discarding (per-clip text preserved)"
                );
                state.clear_pass3_inflight(&paragraph_id_for_task);
                return;
            }
            // Proportional split: assign per-clip text ranges from the
            // returned string based on each clip's duration ratio,
            // snapping cut points to the nearest whitespace.
            let clip_refs: Vec<(String, String)> = window
                .iter()
                .map(|(cid, pid, _, _, _, _)| (cid.clone(), pid.clone()))
                .collect();
            let durations: Vec<u64> = window.iter().map(|(_, _, _, d, _, _)| *d).collect();
            let split_texts = split_boundary_text(&text, &durations);
            let clip_count = clip_refs.len();
            // Per-clip shrinkage guard. Only checks clips we would
            // actually write — the borrowed clip from the previous
            // paragraph is never touched (see the apply loop below),
            // so its shrinkage doesn't count against this check.
            // Rejects the whole update if any writable clip (≥3
            // words) would shrink to < 70% of its current word count.
            let per_clip_shrinkage = window
                .iter()
                .zip(split_texts.iter())
                .filter(|((_, pid, _, _, _, _), _)| pid == &paragraph_id_for_task)
                .any(|((_, _, _, _, old_text, _), new_text)| {
                    let old_w = count_words(old_text);
                    let new_w = count_words(new_text);
                    old_w >= 3 && new_w < (old_w as f64 * 0.7).round() as usize
                });
            if per_clip_shrinkage {
                debug!(
                    paragraph = %paragraph_id_for_task,
                    per_clip = ?window
                        .iter()
                        .zip(split_texts.iter())
                        .filter(|((_, pid, _, _, _, _), _)| pid == &paragraph_id_for_task)
                        .map(|((_, _, _, _, o, _), n)| {
                            format!("{}→{}", count_words(o), count_words(n))
                        })
                        .collect::<Vec<_>>(),
                    "pass3: per-clip shrinkage detected on writable clips — discarding"
                );
                state.clear_pass3_inflight(&paragraph_id_for_task);
                return;
            }
            // Reacquire lock + re-validate.
            let mut g = state.inner.lock().expect("record state mutex poisoned");
            // Locate the trigger paragraph — may have moved.
            let mut hit: Option<(usize, usize)> = None;
            for (ci, ch) in g.channels.iter().enumerate() {
                if let Some(pi) = ch
                    .paragraphs
                    .iter()
                    .position(|p| p.id == paragraph_id_for_task)
                {
                    hit = Some((ci, pi));
                    break;
                }
            }
            let Some((ci, pi)) = hit else {
                debug!(
                    paragraph = %paragraph_id_for_task,
                    "pass3: paragraph gone during decode — discarded"
                );
                return;
            };
            // Every clip must still exist and be non-provisional in
            // its expected paragraph. Cross-paragraph windows count too.
            let clips_still_valid = clip_refs.iter().all(|(cid, pid)| {
                g.channels[ci]
                    .paragraphs
                    .iter()
                    .any(|p| {
                        p.id == *pid
                            && p.clips.iter().any(|c| c.id == *cid && !c.provisional)
                    })
            });
            if !clips_still_valid {
                debug!(
                    paragraph = %paragraph_id_for_task,
                    "pass3: clip set changed during decode — discarded"
                );
                g.channels[ci].paragraphs[pi].pass3_inflight = false;
                return;
            }
            // TEMP DIAGNOSTIC: dump joined re-decode + per-clip old→new
            // so we can see when pass 3's proportional split shuffles
            // content across clip boundaries (the "eats good words" bug).
            info!(
                paragraph = %paragraph_id_for_task,
                joined = %text,
                "pass3-diag: joined re-decode"
            );
            for ((cid, pid, start_wall, dur, old_text, _prov), new_text) in
                window.iter().zip(split_texts.iter())
            {
                let borrowed = pid != &paragraph_id_for_task;
                info!(
                    paragraph = %paragraph_id_for_task,
                    clip = %cid,
                    borrowed,
                    start_wall_ms = start_wall,
                    duration_ms = dur,
                    old = %old_text,
                    new = %new_text,
                    "pass3-diag: per-clip old→new"
                );
            }

            // Apply the split — ONLY to clips in the trigger paragraph.
            // Any borrowed clip from the previous paragraph is context
            // audio only and never gets its text overwritten. This
            // preserves the "once hardened (or done), never amend"
            // invariant even when the pass-3 window happened to span
            // a paragraph seam for smoothing purposes.
            let mut updated_clips: Vec<(String, ClipRef)> = Vec::with_capacity(clip_count);
            let paragraph_snapshot: Option<Paragraph>;
            {
                let paragraph = &mut g.channels[ci].paragraphs[pi];
                for ((cid, pid), new_text) in clip_refs.iter().zip(split_texts.iter()) {
                    if pid != &paragraph_id_for_task {
                        continue;
                    }
                    if let Some(clip) = paragraph.clips.iter_mut().find(|c| c.id == *cid) {
                        clip.text = new_text.clone();
                        clip.pass3_ran = true;
                        updated_clips.push((paragraph_id_for_task.clone(), clip.clone()));
                    }
                }
                paragraph.rebuild();
                paragraph.pass3_ran = true;
                paragraph.pass3_inflight = false;
                // Soft-cap evaluation. Runs on the pass-3-authored text
                // (so trailing punctuation is real, not SenseVoice's
                // per-clip fake period). Looks at the LAST clip that
                // just got a pass-3 rewrite — if IT ends on a sentence
                // and the paragraph is now big enough, mark the whole
                // paragraph "closed" so the next incoming clip opens a
                // fresh one. Skipped for LLM-authored paragraphs
                // (`pass4_ran`) — the LLM owns structural breaks in
                // that mode.
                if !paragraph.pass4_ran
                    && !paragraph.closed
                    && count_words(&paragraph.text) >= PARAGRAPH_SOFT_MAX_WORDS
                {
                    let last_pass3_ends_sentence = paragraph
                        .clips
                        .iter()
                        .rev()
                        .find(|c| c.pass3_ran)
                        .map(|c| ends_on_sentence(&c.text))
                        .unwrap_or(false);
                    if last_pass3_ends_sentence {
                        paragraph.closed = true;
                        debug!(
                            paragraph = %paragraph_id_for_task,
                            words = count_words(&paragraph.text),
                            "pass3: soft-cap hit — paragraph marked closed"
                        );
                    }
                }
                paragraph_snapshot = Some(paragraph.clone());
            }
            // Mirror into the active recording — only the trigger
            // paragraph. Borrowed clips from the previous paragraph
            // were never written above, so nothing to mirror there.
            if let Some(active) = g.active.as_mut() {
                if let Some(mirror) = active
                    .paragraphs
                    .iter_mut()
                    .find(|p| p.id == paragraph_id_for_task)
                {
                    for ((cid, pid), new_text) in clip_refs.iter().zip(split_texts.iter()) {
                        if pid != &paragraph_id_for_task {
                            continue;
                        }
                        if let Some(clip) =
                            mirror.clips.iter_mut().find(|c| c.id == *cid)
                        {
                            clip.text = new_text.clone();
                            clip.pass3_ran = true;
                        }
                    }
                    mirror.rebuild();
                    mirror.pass3_ran = true;
                    mirror.pass3_inflight = false;
                    if let Some(snap) = &paragraph_snapshot {
                        mirror.closed = snap.closed;
                    }
                }
            }
            let recording = g.active.is_some();
            drop(g);
            debug!(
                paragraph = %paragraph_id_for_task,
                clips = clip_count,
                "pass3: applied boundary re-transcription"
            );
            if recording {
                for (pid, clip) in updated_clips {
                    let _ = state.subtitles.send(SubtitleEvent::ClipUpsert {
                        paragraph_id: pid,
                        clip,
                    });
                }
                if let Some(p) = paragraph_snapshot {
                    let _ = state.subtitles.send(SubtitleEvent::ParagraphUpsert(p));
                }
            }
        });
    }
}

/// Result of `RecordState::take_active`. Owned buffers so the caller can
/// hand them straight to the WAV encoder / store without holding the state
/// lock.
pub struct TakenRecording {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub paragraphs: Vec<Paragraph>,
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

/// Outcome of a clip removal in a paragraph list. The caller uses this to
/// pick between broadcasting a `ClipRemove` (paragraph still exists) or
/// `ParagraphRemove` (last clip in the paragraph fell out).
enum RemoveOutcome {
    ClipGone {
        paragraph_id: String,
        clip_id: String,
    },
    ParagraphGone(String),
}

/// Locate `clip_id` inside a paragraph list, replace its text, rebuild
/// the paragraph, and return `(paragraph_id, new clip)` on hit. `None`
/// when the clip id isn't present.
fn update_clip_in_paragraphs(
    paragraphs: &mut [Paragraph],
    clip_id: &str,
    text: &str,
) -> Option<(String, ClipRef)> {
    for paragraph in paragraphs.iter_mut() {
        if let Some(clip) = paragraph.clips.iter_mut().find(|c| c.id == clip_id) {
            clip.text = text.to_string();
            let updated = clip.clone();
            paragraph.rebuild();
            return Some((paragraph.id.clone(), updated));
        }
    }
    None
}

/// Locate `clip_id` inside a paragraph list and remove it, dropping the
/// entire paragraph if that leaves it empty. Returns the outcome so the
/// caller can broadcast the right event.
fn remove_clip_from_paragraphs(
    paragraphs: &mut Vec<Paragraph>,
    clip_id: &str,
) -> Option<RemoveOutcome> {
    for (pidx, paragraph) in paragraphs.iter_mut().enumerate() {
        if let Some(cidx) = paragraph.clips.iter().position(|c| c.id == clip_id) {
            paragraph.clips.remove(cidx);
            if paragraph.clips.is_empty() {
                let pid = paragraph.id.clone();
                paragraphs.remove(pidx);
                return Some(RemoveOutcome::ParagraphGone(pid));
            }
            paragraph.rebuild();
            return Some(RemoveOutcome::ClipGone {
                paragraph_id: paragraph.id.clone(),
                clip_id: clip_id.to_string(),
            });
        }
    }
    None
}

/// Trim `ch.paragraphs` back to the FIFO cap. Called after every append
/// so a long idle process can't accumulate unbounded paragraphs.
fn trim_channel_cap(ch: &mut ChannelState) {
    while ch.paragraphs.len() > CHANNEL_PARAGRAPH_CAP {
        ch.paragraphs.remove(0);
    }
}

/// Core upsert helper shared by pass-1 (`upsert_provisional_clip`) and
/// pass-2 (`finalize_clip`). Handles paragraph gap detection, in-place
/// clip updates by id, and paragraph creation when the gap trips or the
/// channel is empty.
///
/// Returns:
///   * `Option<Paragraph>` — populated when a fresh paragraph was
///     opened, so the caller can broadcast a `ParagraphUpsert`.
///   * `ClipRef` — the (possibly-updated) clip, ready to broadcast in a
///     `ClipUpsert`.
///   * `String` — the paragraph id owning that clip.
#[allow(clippy::too_many_arguments)]
fn upsert_clip_into_channel(
    ch: &mut ChannelState,
    clip_id: &str,
    text: &str,
    audio_start_ms: Option<u64>,
    audio_duration_ms: Option<u64>,
    speaker: &Option<String>,
    start_wall_ms: u64,
    channel_name: &str,
    provisional: bool,
) -> (Option<Paragraph>, ClipRef, String) {
    // First: if the clip id already exists in any paragraph on this
    // channel, mutate in place. This is the streaming-partial → final
    // handoff plus the pass-1 partial-growth case.
    //
    // Guard: once a clip has been finalized (`provisional=false` — set
    // by `finalize_clip`), any subsequent provisional update is a stale
    // partial. This happens when the streaming decoder's mpsc still has
    // queued `Feed` commands for a clip whose offline SenseVoice
    // decode won the race and already landed `finalize_clip`. Ignoring
    // stale partials keeps the polished text and the state indicator
    // from regressing back to "streaming".
    for paragraph in ch.paragraphs.iter_mut() {
        if let Some(clip) = paragraph.clips.iter_mut().find(|c| c.id == clip_id) {
            if provisional && !clip.provisional {
                let unchanged = clip.clone();
                return (None, unchanged, paragraph.id.clone());
            }
            clip.text = text.to_string();
            clip.audio_start_ms = audio_start_ms;
            clip.audio_duration_ms = audio_duration_ms;
            clip.start_wall_ms = start_wall_ms;
            clip.provisional = provisional;
            let updated = clip.clone();
            paragraph.rebuild();
            return (None, updated, paragraph.id.clone());
        }
    }
    // Otherwise: decide append vs new paragraph. New paragraph fires
    // when the channel is empty OR the previous paragraph's last clip
    // ended more than PARAGRAPH_GAP_MS before this new clip's start.
    //
    // Subtlety: on continuous speech that trips VAD's max-length
    // (`VAD_MAX_UTTERANCE_MS`), the next clip's provisional partial can
    // arrive BEFORE the previous clip's offline STT finalize task fills
    // in `audio_duration_ms`. Naïvely treating a None duration as zero
    // duration would make the gap look 15+ seconds long — and every
    // max-length rollover would spuriously open a new paragraph mid-
    // monologue. When the previous clip is still provisional, bound
    // last_end conservatively by assuming the utterance ran up to its
    // VAD max length. Real silence longer than PARAGRAPH_GAP_MS still
    // opens a new paragraph (gap > VAD_MAX + PARAGRAPH_GAP_MS).
    let need_new = match ch.paragraphs.last() {
        None => true,
        Some(last_p) => {
            if last_p.closed {
                // Soft-cap was hit in the pass-3 completion path; treat
                // any new clip on this channel as a fresh paragraph.
                true
            } else {
                match last_p.clips.last() {
                    None => true,
                    Some(last_clip) => {
                        let last_end = match last_clip.audio_duration_ms {
                            Some(d) => last_clip.start_wall_ms.saturating_add(d),
                            None if last_clip.provisional => last_clip
                                .start_wall_ms
                                .saturating_add(VAD_MAX_UTTERANCE_MS as u64),
                            None => last_clip.start_wall_ms,
                        };
                        start_wall_ms.saturating_sub(last_end) > PARAGRAPH_GAP_MS
                    }
                }
            }
        }
    };
    let clip = ClipRef {
        id: clip_id.to_string(),
        audio_start_ms,
        audio_duration_ms,
        start_wall_ms,
        text: text.to_string(),
        provisional,
        audio_url: None,
        mixed_start_ms: None,
        pass3_ran: false,
        pass4_ran: false,
    };
    if need_new {
        // First-clip speaker attribution stays on the paragraph for its
        // lifetime; subsequent clips don't override it. Per-channel = one
        // speaker in Stage 1.
        let mut paragraph = Paragraph {
            id: Uuid::new_v4().to_string(),
            channel: channel_name.to_string(),
            speaker: speaker.clone(),
            hardened: false,
            text: String::new(),
            raw_text: String::new(),
            clips: vec![clip.clone()],
            start_wall_ms,
            end_wall_ms: start_wall_ms,
            created_at: unix_now(),
            pass3_ran: false,
            pass4_ran: false,
            pass3_inflight: false,
            closed: false,
            edits: Vec::new(),
            deleted: false,
        };
        paragraph.rebuild();
        ch.paragraphs.push(paragraph.clone());
        trim_channel_cap(ch);
        // Re-locate the paragraph we just pushed (trim_channel_cap may
        // have shifted indices) so we return the canonical stored copy.
        let stored = ch
            .paragraphs
            .last()
            .cloned()
            .unwrap_or_else(|| paragraph.clone());
        let pid = stored.id.clone();
        (Some(stored), clip, pid)
    } else {
        let last = ch.paragraphs.last_mut().expect("checked above");
        last.clips.push(clip.clone());
        last.rebuild();
        (None, clip, last.id.clone())
    }
}

/// Mirror an upsert (either an existing-clip edit or a fresh paragraph)
/// into the active recording's paragraph list.
///
/// * `new_paragraph = Some(_)` means the caller just opened a fresh
///   paragraph on the channel side; clone it into `active.paragraphs`.
/// * `new_paragraph = None` means the clip landed inside an existing
///   paragraph on the channel side — find that paragraph by id in the
///   active recording (mirroring an earlier push) and update/append the
///   clip there. If the paragraph isn't mirrored yet (rare — an edit to
///   a paragraph that started before this recording), the clip is
///   dropped for the active view.
fn mirror_upsert_into_active(
    active: &mut ActiveRecording,
    paragraph_id: &str,
    clip: &ClipRef,
    new_paragraph: &Option<Paragraph>,
) {
    if let Some(p) = new_paragraph {
        active.paragraphs.push(p.clone());
        return;
    }
    for paragraph in active.paragraphs.iter_mut() {
        if paragraph.id != paragraph_id {
            continue;
        }
        if let Some(existing) = paragraph.clips.iter_mut().find(|c| c.id == clip.id) {
            *existing = clip.clone();
        } else {
            paragraph.clips.push(clip.clone());
        }
        paragraph.rebuild();
        return;
    }
}

/// Mark the second-to-last paragraph on the given Vox channel as
/// hardened. Called from `upsert_provisional_clip` /
/// `finalize_clip` when a new paragraph has just been pushed and the
/// operator is in live-buffer + LLM-off mode: the just-closed
/// paragraph won't see any more processing, so surface that finality
/// via the UI's state dot.
///
/// No-op when the channel has fewer than two paragraphs (nothing to
/// harden yet). We don't mirror into `active` here — this path only
/// runs when `!is_recording()`, so `active` is `None` by construction.
fn harden_prev_paragraph(inner: &mut Inner, kind: ChannelKind) {
    let Some(ch) = inner.channel_mut(kind) else {
        return;
    };
    let n = ch.paragraphs.len();
    if n < 2 {
        return;
    }
    ch.paragraphs[n - 2].hardened = true;
}

/// Known SenseVoice single-word false-positive outputs. When the model
/// is handed a short, marginal segment (a mic bump, a Discord notification
/// chirp, one syllable of background noise) it often "hallucinates" one
/// of these very short bookend phrases. A real utterance of just "I." or
/// "The." isn't meaningful without the rest of the sentence, so the
/// operator loses nothing by dropping them. English matches are exact
/// (trimmed); broader stop-lists risk swallowing legitimate one-word
/// replies. For non-Latin scripts SenseVoice hallucinates short
/// interjection characters (e.g. `啊。`, `嗯`, `哦.`) on the same kinds
/// of marginal segments; those are dropped by a length heuristic when
/// the text is a single "word" (no whitespace) with no Latin letters
/// and at most two script characters.
/// Count whitespace-separated words in `text`. Used by the paragraph
/// soft-cap check to decide when a paragraph is big enough that a
/// sentence-boundary break would improve readability. Cheap linear
/// scan; the soft-cap runs at most once per clip finalize.
fn count_words(text: &str) -> usize {
    text.split_whitespace().count()
}

/// True when `text`'s last non-whitespace character is a
/// sentence-terminating punctuation mark. Handles the standard ASCII
/// set (`.`, `!`, `?`) plus the fullwidth CJK equivalents SenseVoice
/// emits on Chinese / Japanese output. Used by the paragraph soft-cap
/// to only cut at real sentence ends after pass 3 has consolidated
/// the trailing clip.
fn ends_on_sentence(text: &str) -> bool {
    matches!(
        text.trim_end().chars().last(),
        Some('.' | '!' | '?' | '。' | '！' | '？'),
    )
}

fn is_false_positive(text: &str) -> bool {
    let trimmed = text.trim();
    if matches!(trimmed, "I." | "The.") {
        return true;
    }
    if trimmed.chars().any(char::is_whitespace) {
        return false;
    }
    if trimmed.chars().any(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    let content_chars = trimmed
        .chars()
        .filter(|c| c.is_alphanumeric())
        .count();
    content_chars > 0 && content_chars <= 2
}

/// Proportional split of a pass-3 result string across the window's
/// clips by duration ratio. Cut points snap to the nearest whitespace
/// so multi-char words never split across two clips. `durations` and
/// the returned Vec have the same length; a run of empty durations or
/// an empty `text` collapses every slot to `""` (the caller then
/// leaves clip text untouched at rebuild time — the resulting empty
/// paragraph would get pruned by later stages).
fn split_boundary_text(text: &str, durations: &[u64]) -> Vec<String> {
    let n = durations.len();
    if n == 0 {
        return Vec::new();
    }
    let text = text.trim();
    if text.is_empty() {
        return vec![String::new(); n];
    }
    let total: u64 = durations.iter().sum();
    if total == 0 {
        // No duration info — dump everything into the last clip.
        let mut out = vec![String::new(); n];
        out[n - 1] = text.to_string();
        return out;
    }
    // Work in char indices (not byte indices) so multi-byte UTF-8 is
    // handled correctly. `char_positions` is the byte offset of each
    // char, plus a trailing sentinel at `text.len()`.
    let char_positions: Vec<usize> = text
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()))
        .collect();
    let total_chars = char_positions.len() - 1;
    // Nearest-whitespace snap: find the closest whitespace char to
    // `target_char_idx`, searching outward up to `half_word_width`
    // chars in either direction. Returns a char index (into
    // `char_positions`) suitable for slicing.
    let half_word_width = ((total_chars as u64) / (n as u64 * 2)).max(1) as usize;
    let chars: Vec<char> = text.chars().collect();
    let snap_to_whitespace = |target: usize| -> usize {
        if target == 0 || target >= total_chars {
            return target.min(total_chars);
        }
        // Prefer the closest whitespace within the half-word window.
        for d in 0..=half_word_width {
            let left = target.saturating_sub(d);
            let right = (target + d).min(total_chars);
            // A char index `i` sits on a whitespace boundary when
            // chars[i-1] is whitespace (i.e. the split-before happens
            // at start of a non-space run).
            if left > 0 && chars[left - 1].is_whitespace() {
                return left;
            }
            if right < total_chars && chars[right - 1].is_whitespace() {
                return right;
            }
            if right == total_chars {
                return right;
            }
        }
        target.min(total_chars)
    };
    let mut out: Vec<String> = Vec::with_capacity(n);
    let mut prev_char_end = 0usize;
    let mut acc: u64 = 0;
    for (i, dur) in durations.iter().enumerate() {
        let text_slice = if i + 1 == n {
            // Last clip: whatever remains.
            let start_byte = char_positions[prev_char_end];
            text[start_byte..].trim().to_string()
        } else {
            acc = acc.saturating_add(*dur);
            let frac_end =
                ((acc as f64 / total as f64) * total_chars as f64).round() as usize;
            let frac_end = frac_end.min(total_chars);
            let char_end = snap_to_whitespace(frac_end).max(prev_char_end);
            let start_byte = char_positions[prev_char_end];
            let end_byte = char_positions[char_end];
            prev_char_end = char_end;
            text[start_byte..end_byte].trim().to_string()
        };
        out.push(text_slice);
    }
    out
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

/// Millisecond-precision unix wall-clock timestamp.
fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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
    /// polished final itself via [`RecordState::finalize_clip`],
    /// which replaces whatever provisional partial the streaming
    /// decoder last broadcast.
    EndUtterance,
    /// Discard the current utterance without emitting a final —
    /// removes any partial rows already broadcast for its `clip_id`.
    /// Used when the VAD closes a too-short utterance we've decided
    /// to drop as noise.
    Discard,
}

/// Metadata captured at speech-start time. `clip_id` is generated by
/// the VAD worker so partials + the eventual final share one row.
struct StreamStart {
    clip_id: String,
    slot: usize,
    audio_start_ms: Option<u64>,
    start_wall_ms: u64,
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
    current_clip_id: Option<String>,
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
    /// Wall-clock unix ms at speech-start. Used as the paragraph's
    /// start_wall_ms so partials land at their true position in the log
    /// instead of jumping around as more text arrives.
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
            current_clip_id: None,
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
                        let clip_id = Uuid::new_v4().to_string();
                        self.current_clip_id = Some(clip_id.clone());
                        let start_ms = self.utterance_start_frame * 1000
                            / self.sample_rate as u64;
                        let _ = tx.try_send(StreamCmd::Start(StreamStart {
                            clip_id,
                            slot: self.slot,
                            audio_start_ms: Some(start_ms),
                            start_wall_ms: self.utterance_start_wall_ms,
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
    /// its output via `state.finalize_clip(...)` (pass 2). If streaming
    /// wasn't configured, no provisional clip exists — the finalize call
    /// will create one directly.
    fn finalize_utterance(&mut self, reason: &'static str) {
        let samples = std::mem::take(&mut self.utterance);
        let start_frame = self.utterance_start_frame;
        let start_wall_ms = self.utterance_start_wall_ms;
        let current_clip_id = self.current_clip_id.take();
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
            if let Some(id) = &current_clip_id {
                state.remove_clip(id);
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
        // callback lands the polished text via `finalize_clip`.
        // Whether or not a streaming partial exists for this clip,
        // `finalize_clip` upserts by id: an existing provisional row is
        // updated in place, otherwise a new final clip is inserted.
        let stt = self.stt.clone();
        let start_wall_ms = if start_wall_ms == 0 {
            unix_now_ms().saturating_sub(duration_ms)
        } else {
            start_wall_ms
        };
        // If no streaming clip id was assigned (streaming disabled), mint
        // one now so `finalize_clip` has a stable key. The clip lands as
        // a fresh final in whatever paragraph the gap logic picks.
        let clip_id = current_clip_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        tokio::spawn(async move {
            let res = tokio::task::spawn_blocking(move || stt.transcribe(&samples, sr)).await;
            match res {
                Ok(Ok(text)) => {
                    let trimmed = text.trim().to_string();
                    state.finalize_clip(
                        slot,
                        &clip_id,
                        trimmed,
                        Some(start_ms),
                        Some(duration_ms),
                        speaker,
                        start_wall_ms,
                    );
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
/// `state.upsert_provisional_clip` after each Feed that materially
/// changes the text; on `EndUtterance` the sherpa stream is reset but no
/// final is emitted — the async VAD worker owns re-transcription via the
/// offline (higher-accuracy) SenseVoice model and replaces the last
/// partial itself via `state.finalize_clip`.
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
                    state.upsert_provisional_clip(
                        start.slot,
                        &start.clip_id,
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
                    state.remove_clip(&start.clip_id);
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
                        // Feed the per-slot audio ring unconditionally,
                        // even for disabled slots. Pass 3 wants an
                        // ambient audio history so paragraph
                        // consolidation still works if the operator
                        // toggles enable mid-utterance.
                        state.push_slot_audio(slot, &chunk);
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
                        if let Some(id) = worker.current_clip_id.take() {
                            worker.state.remove_clip(&id);
                        }
                        // Also mark the audio ring as post-lagged so a
                        // pass-3 request can't stitch pre- and post-lag
                        // audio into one buffer with a hidden gap.
                        state.mark_slot_lagged(slot);
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
