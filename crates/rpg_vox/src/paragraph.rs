//! Pass-4 LLM paragraph reorganization.
//!
//! The Live-transcription pipeline emits provisional clips (pass 1) and
//! finalized clips (pass 2) into [`crate::record::Paragraph`] containers
//! that grow inside a per-channel hot zone. Pass 3
//! ([`crate::record::RecordState::schedule_boundary_retranscribe`])
//! re-decodes the last few clips as one audio span so word-splits at
//! utterance boundaries land at real word boundaries in the text.
//!
//! Pass 4 (this module) then feeds the hot zone to an LLM which returns
//! a fresh list of paragraph groupings — the LLM may edit text, merge
//! adjacent utterance fragments into one paragraph, or split a long
//! run into two paragraphs at a semantic break. Once a paragraph
//! scrolls out of the last `HOT_ZONE_N` positions of the LLM's output
//! it becomes **hardened** and never gets fed through the LLM again;
//! future runs treat it as read-only context.
//!
//! Scheduling shape: one long-lived `tokio` task per Vox channel. All
//! calls on that channel are automatically serialized (single consumer
//! per mpsc receiver). TTS clips never fire the scheduler — their text
//! is already LLM-authored.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::chat::{self, ChatMessage};
use crate::record::{ClipRef, Paragraph, RecordState};

/// Maximum number of paragraphs kept in the LLM's editable hot zone.
/// Older paragraphs (past this count from the tail) are treated as
/// hardened and are read-only context for future LLM calls.
pub const HOT_ZONE_N: usize = 3;
/// Maximum number of already-hardened paragraphs included as read-only
/// context in the LLM prompt. Older ones are dropped — three feels like
/// enough to give the LLM a running gist of the conversation without
/// blowing the prompt budget.
pub const HARDENED_CONTEXT_N: usize = 3;
/// Minimum wall-clock spacing between LLM calls on the same channel.
/// The scheduler task waits out the remainder of this window if a
/// trigger fires sooner. `ForcedBreak` honors the debounce too — the
/// spec says debounce paces timing, not whether the call happens.
pub const LLM_DEBOUNCE_MS: u64 = 3_000;
/// Word count on the unassigned-to-hot material (new clips finalized
/// past the last hot paragraph's `end_wall_ms`) that triggers an LLM
/// call. Either this OR the audio-duration threshold below is enough.
pub const LLM_WORD_TRIGGER: u32 = 40;
/// Total wall-clock duration (ms) of unassigned-to-hot material that
/// triggers an LLM call.
pub const LLM_AUDIO_TRIGGER_MS: u64 = 15_000;

/// One trigger fired by `finalize_clip` (or by a silence-gap paragraph
/// close) at the per-channel scheduler task. `Wake` says "consider
/// firing"; `ForcedBreak` says "consider firing soon regardless of the
/// debounce ceiling" (a silence-closed paragraph is a coherent moment
/// worth reorganizing at).
#[derive(Debug, Clone, Copy)]
pub enum LlmTrigger {
    Wake,
    /// A > PARAGRAPH_GAP_MS silence just closed a paragraph on this
    /// channel. Still honors debounce (per spec) — the flag is used
    /// only to coalesce with any queued `Wake`s.
    ForcedBreak,
}

/// Per-channel senders into the LLM scheduler tasks. Held on
/// [`RecordState`] via `set_llm_scheduler`. Slot `i` corresponds to Vox
/// slot `i`; TTS has no entry (never scheduled).
pub struct LlmScheduler {
    senders: Vec<Option<mpsc::Sender<LlmTrigger>>>,
}

impl LlmScheduler {
    /// Fire a `Wake` at slot `slot`. Silently drops when the slot is
    /// out of range, when the sender is `None` (TTS or unconfigured),
    /// or when the channel is full — a queued `Wake` already covers
    /// the "consider firing" case, and coalescing means we don't need
    /// to backpressure.
    pub fn wake(&self, slot: usize) {
        let Some(Some(tx)) = self.senders.get(slot) else {
            return;
        };
        let _ = tx.try_send(LlmTrigger::Wake);
    }

    /// Fire a `ForcedBreak` at slot `slot`. Same drop semantics as
    /// [`Self::wake`]; a full channel means the task is behind but the
    /// most-recent trigger before the drain will still be seen.
    pub fn forced_break(&self, slot: usize) {
        let Some(Some(tx)) = self.senders.get(slot) else {
            return;
        };
        let _ = tx.try_send(LlmTrigger::ForcedBreak);
    }

    /// `true` when no channels have an active sender (e.g. TTS-only
    /// build). Currently unused; kept for future health checks.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.senders.iter().all(|s| s.is_none())
    }
}

/// Spawn one long-lived tokio task per Vox channel. Each task holds a
/// single mpsc receiver, which serializes LLM calls for that channel;
/// different channels run in parallel. TTS is never scheduled — the
/// TTS entry in the returned scheduler's `senders` is `None`.
///
/// Callers should register the returned scheduler on the record state
/// via [`RecordState::set_llm_scheduler`].
pub fn spawn(
    record: RecordState,
    chat: chat::Client,
    channel_count: usize,
) -> LlmScheduler {
    let mut senders: Vec<Option<mpsc::Sender<LlmTrigger>>> =
        Vec::with_capacity(channel_count + 1);
    for slot in 0..channel_count {
        let (tx, rx) = mpsc::channel::<LlmTrigger>(8);
        let record = record.clone();
        let chat = chat.clone();
        tokio::spawn(run_channel_task(record, chat, slot, rx));
        senders.push(Some(tx));
    }
    // TTS pseudo-channel entry — no task, no sender. Slot is reserved
    // so a caller indexing by ChannelKind::Tts semantics stays sane.
    senders.push(None);
    LlmScheduler { senders }
}

/// One channel's LLM scheduler loop. Reads triggers, applies debounce,
/// runs the LLM call, applies the diff. Never returns errors — logs on
/// failure and continues.
async fn run_channel_task(
    record: RecordState,
    chat: chat::Client,
    slot: usize,
    mut rx: mpsc::Receiver<LlmTrigger>,
) {
    let mut last_llm_at: Option<Instant> = None;
    loop {
        // Block until at least one trigger arrives.
        let Some(first) = rx.recv().await else {
            debug!(slot, "paragraph: scheduler rx closed; exiting");
            return;
        };
        // Coalesce any pending triggers. `ForcedBreak` beats `Wake`
        // — if either kind is present, treat the whole batch as a
        // forced break.
        let mut forced = matches!(first, LlmTrigger::ForcedBreak);
        while let Ok(t) = rx.try_recv() {
            if matches!(t, LlmTrigger::ForcedBreak) {
                forced = true;
            }
        }
        // Debounce. Even ForcedBreak waits — per spec, debounce paces
        // timing, not whether the call happens.
        if let Some(last) = last_llm_at {
            let elapsed = last.elapsed();
            let debounce = Duration::from_millis(LLM_DEBOUNCE_MS);
            if elapsed < debounce {
                tokio::time::sleep(debounce - elapsed).await;
            }
        }
        // Attempt to fire. `run_once` returns whether an LLM call
        // actually landed; only actual fires bump `last_llm_at`.
        match run_once(&record, &chat, slot, forced).await {
            LlmOutcome::Fired => {
                last_llm_at = Some(Instant::now());
            }
            LlmOutcome::Skipped(reason) => {
                debug!(slot, reason, "paragraph: skipped LLM cycle");
            }
            LlmOutcome::Aborted(reason) => {
                warn!(slot, reason, "paragraph: aborted LLM cycle");
            }
        }
        // Drain any additional triggers that arrived DURING the call;
        // if any did, immediately loop with no debounce wait — those
        // triggers already waited their share of time.
        while let Ok(_t) = rx.try_recv() {
            // Consumed; the loop iteration above will handle the next
            // recv (which is now cheap since data may or may not be
            // waiting).
        }
    }
}

enum LlmOutcome {
    Fired,
    Skipped(&'static str),
    Aborted(&'static str),
}

/// Snapshot of a channel's state at the moment we build the prompt.
/// Lives outside the state lock so the async LLM call never holds it.
struct ChannelSnapshot {
    /// Number of hardened paragraphs left in place (their positions
    /// are stable and won't be touched by the diff-apply).
    hardened_count: usize,
    /// Position in `channel.paragraphs` where the hot zone begins.
    /// Equal to `hardened_count` at snapshot time.
    hot_start_index: usize,
    /// Read-only context (up to `HARDENED_CONTEXT_N`, newest-last).
    context: Vec<Paragraph>,
    /// Editable hot paragraphs (up to `HOT_ZONE_N`, newest-last).
    hot: Vec<Paragraph>,
    /// Whether any new material past the last hot paragraph exists.
    /// Currently always false because Stage 3 uses per-clip appending
    /// through `upsert_clip_into_channel` — new clips fold into the
    /// last hot paragraph rather than creating unassigned clips.
    /// Retained for future-proofing (see doc-comment on
    /// [`compute_unassigned_metrics`]).
    #[allow(dead_code)]
    new_material: Vec<UnassignedClip>,
}

#[derive(Debug, Clone)]
struct UnassignedClip {
    id: String,
    text: String,
    gap_ms_since_prev: u64,
}

/// One decision cycle. Returns whether we actually fired the LLM.
async fn run_once(
    record: &RecordState,
    chat: &chat::Client,
    slot: usize,
    forced: bool,
) -> LlmOutcome {
    // Snapshot channel state under the state lock, then drop it before
    // any async work.
    let snapshot = {
        let Some(paragraphs) = record.snapshot_channel_paragraphs(slot) else {
            return LlmOutcome::Skipped("slot out of range");
        };
        build_snapshot(&paragraphs)
    };

    // Empty hot zone AND no new material → nothing to reorganize.
    if snapshot.hot.is_empty() && snapshot.new_material.is_empty() {
        return LlmOutcome::Skipped("empty hot zone");
    }

    // Threshold gate. Forced-breaks bypass the word/audio thresholds
    // entirely (they're a "we just closed a paragraph, please
    // reconsider" signal). Otherwise the scheduler task itself decides
    // whether the unassigned material is worth the LLM cost.
    if !forced && !thresholds_met(&snapshot) {
        return LlmOutcome::Skipped("thresholds not met");
    }

    // Build the prompt.
    let user_payload = build_user_payload(&snapshot);
    let user_json = match serde_json::to_string(&user_payload) {
        Ok(s) => s,
        Err(err) => {
            warn!(slot, err = %err, "paragraph: prompt serialization failed");
            return LlmOutcome::Aborted("prompt serialization failed");
        }
    };
    let history = vec![ChatMessage {
        role: "user".into(),
        content: user_json,
    }];
    let schema = response_schema();

    // First attempt.
    let attempt = chat
        .generate_reply_json(history.clone(), Some(SYSTEM_PROMPT.into()), schema.clone())
        .await;
    let output_paragraphs = match parse_and_validate(attempt, &snapshot) {
        Ok(paragraphs) => paragraphs,
        Err(reason) => {
            // Retry once with the error appended to a system-prompt
            // override addendum. If that also fails, abort without
            // touching state.
            let addendum = format!(
                "{SYSTEM_PROMPT}\n\nYour previous response was rejected: {reason}. \
                 Return a valid JSON array matching the schema."
            );
            let retry = chat
                .generate_reply_json(history, Some(addendum), response_schema())
                .await;
            match parse_and_validate(retry, &snapshot) {
                Ok(paragraphs) => paragraphs,
                Err(final_reason) => {
                    warn!(slot, reason = %final_reason, "paragraph: LLM cycle rejected twice");
                    return LlmOutcome::Aborted("LLM output invalid after retry");
                }
            }
        }
    };

    // Apply the diff. Reacquire the lock via `apply_llm_diff` which
    // handles the "hot zone changed while we were decoding" case by
    // clamping the truncation index.
    apply_diff(record, slot, &snapshot, output_paragraphs);
    LlmOutcome::Fired
}

/// Hardcoded system prompt. No CLI knob in Stage 3.
const SYSTEM_PROMPT: &str = "You are a paragraph editor for a live transcription pipeline. You receive:\n\
- Up to 3 previous PARAGRAPHS that are already hardened (context only — do not edit them).\n\
- 0 to 3 HOT paragraphs that are provisional and MUST be re-emitted (you may edit their text and split/merge them).\n\
- A stream of raw CLIP fragments that come after the last hot paragraph.\n\
\n\
Your job: emit paragraphs of continuous prose that read well. Fix obvious transcription errors from context. Respect silence-gap boundaries — any clip pair with `gap_ms_since_prev` greater than 6000 MUST have a paragraph break between them.\n\
\n\
Output: a JSON array of objects. Each object has:\n\
- `end_clip`: the clip id of the LAST clip contained in the paragraph.\n\
- `text`: the paragraph's final prose text.\n\
\n\
Every clip id in the input (from HOT + new material) must appear in exactly one output paragraph's `end_clip` (or be included as an internal clip of a paragraph whose `end_clip` is later than it). The clip ids in your `end_clip` values must be in the same order as they appear in the input.";

/// JSON schema handed to `response_format`. Kept as a Value so
/// llama.cpp's grammar-constrained decoding sees the exact structure
/// we validate against.
fn response_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "end_clip": {"type": "string"},
                "text": {"type": "string"}
            },
            "required": ["end_clip", "text"],
            "additionalProperties": false
        }
    })
}

/// User-facing payload structure (single JSON object per call).
#[derive(serde::Serialize)]
struct UserPayload<'a> {
    context: Vec<ContextParagraph<'a>>,
    hot: Vec<HotParagraph<'a>>,
    new_material: Vec<NewClip<'a>>,
}

#[derive(serde::Serialize)]
struct ContextParagraph<'a> {
    id: &'a str,
    text: &'a str,
}

#[derive(serde::Serialize)]
struct HotParagraph<'a> {
    id: &'a str,
    clips: Vec<HotClip<'a>>,
}

#[derive(serde::Serialize)]
struct HotClip<'a> {
    clip_id: &'a str,
    text: &'a str,
    gap_ms_since_prev: u64,
}

#[derive(serde::Serialize)]
struct NewClip<'a> {
    clip_id: &'a str,
    text: &'a str,
    gap_ms_since_prev: u64,
}

/// Compute the hardened/hot split of a channel's paragraph list.
fn build_snapshot(paragraphs: &[Paragraph]) -> ChannelSnapshot {
    // Hot zone = the last up-to-HOT_ZONE_N NON-hardened paragraphs at
    // the tail. Everything before that stays put.
    //
    // Walk from the tail collecting up to HOT_ZONE_N paragraphs whose
    // `hardened=false`. Anything left of that is either hardened or
    // implicitly hardened by scrolling past.
    let mut hot: Vec<Paragraph> = Vec::with_capacity(HOT_ZONE_N);
    let mut hot_start = paragraphs.len();
    for (i, p) in paragraphs.iter().enumerate().rev() {
        if p.hardened {
            break;
        }
        if hot.len() >= HOT_ZONE_N {
            break;
        }
        hot.push(p.clone());
        hot_start = i;
    }
    hot.reverse(); // newest-last for prompt shape

    let hardened_count = hot_start;
    // Read-only context = up to HARDENED_CONTEXT_N paragraphs
    // immediately before the hot zone, newest-last.
    let ctx_start = hardened_count.saturating_sub(HARDENED_CONTEXT_N);
    let context: Vec<Paragraph> = paragraphs[ctx_start..hardened_count].to_vec();

    ChannelSnapshot {
        hardened_count,
        hot_start_index: hot_start,
        context,
        hot,
        new_material: Vec::new(),
    }
}

/// Decide whether the unassigned material past the last hot paragraph
/// warrants an LLM call. Stage 3 folds every finalized clip into the
/// current hot paragraph (`upsert_clip_into_channel` appends within
/// the gap window), so "new material past the last hot paragraph" is
/// always empty in this codebase — but the LLM's editing scope covers
/// the entire hot zone anyway, so we treat any material in the hot
/// zone as "new" for threshold purposes.
///
/// Sums word counts and audio duration across every clip in the hot
/// zone plus any dangling unassigned clips.
fn thresholds_met(snapshot: &ChannelSnapshot) -> bool {
    let (words, audio_ms) = compute_unassigned_metrics(snapshot);
    words >= LLM_WORD_TRIGGER || audio_ms >= LLM_AUDIO_TRIGGER_MS
}

/// Return `(word_count, total_audio_ms)` across the hot zone + new
/// material. See [`thresholds_met`] for why the hot zone counts.
fn compute_unassigned_metrics(snapshot: &ChannelSnapshot) -> (u32, u64) {
    let mut words: u32 = 0;
    let mut audio_ms: u64 = 0;
    for p in &snapshot.hot {
        for c in &p.clips {
            words = words.saturating_add(count_words(&c.text));
            audio_ms = audio_ms.saturating_add(c.audio_duration_ms.unwrap_or(0));
        }
    }
    for c in &snapshot.new_material {
        words = words.saturating_add(count_words(&c.text));
        audio_ms = audio_ms.saturating_add(c.gap_ms_since_prev); // best-effort
    }
    (words, audio_ms)
}

fn count_words(s: &str) -> u32 {
    s.split_whitespace().count() as u32
}

/// Compute the gap in wall-clock ms between one clip and its
/// predecessor. First clip in the paragraph returns 0. Uses the
/// previous clip's end (start + duration).
fn gap_ms_between(prev: Option<&ClipRef>, cur: &ClipRef) -> u64 {
    let Some(prev) = prev else { return 0 };
    let prev_end = prev
        .audio_duration_ms
        .map(|d| prev.start_wall_ms.saturating_add(d))
        .unwrap_or(prev.start_wall_ms);
    cur.start_wall_ms.saturating_sub(prev_end)
}

/// Build the user-payload JSON structure from a snapshot.
fn build_user_payload(snapshot: &ChannelSnapshot) -> UserPayload<'_> {
    let context: Vec<ContextParagraph<'_>> = snapshot
        .context
        .iter()
        .map(|p| ContextParagraph {
            id: p.id.as_str(),
            text: p.text.as_str(),
        })
        .collect();
    let hot: Vec<HotParagraph<'_>> = snapshot
        .hot
        .iter()
        .map(|p| {
            let mut clips: Vec<HotClip<'_>> = Vec::with_capacity(p.clips.len());
            for (i, c) in p.clips.iter().enumerate() {
                let prev = if i == 0 { None } else { Some(&p.clips[i - 1]) };
                clips.push(HotClip {
                    clip_id: c.id.as_str(),
                    text: c.text.as_str(),
                    gap_ms_since_prev: gap_ms_between(prev, c),
                });
            }
            HotParagraph {
                id: p.id.as_str(),
                clips,
            }
        })
        .collect();
    let new_material: Vec<NewClip<'_>> = snapshot
        .new_material
        .iter()
        .map(|c| NewClip {
            clip_id: c.id.as_str(),
            text: c.text.as_str(),
            gap_ms_since_prev: c.gap_ms_since_prev,
        })
        .collect();
    UserPayload {
        context,
        hot,
        new_material,
    }
}

/// One paragraph in the LLM's response.
#[derive(Debug, Clone)]
struct LlmParagraph {
    end_clip: String,
    text: String,
}

/// Parse + validate the LLM's response against the snapshot. On any
/// error, return a short `&'static str` describing the class of
/// failure — used to build the retry-prompt addendum.
fn parse_and_validate(
    attempt: anyhow::Result<serde_json::Value>,
    snapshot: &ChannelSnapshot,
) -> Result<Vec<LlmParagraph>, String> {
    let value = match attempt {
        Ok(v) => v,
        Err(err) => return Err(format!("chat call failed: {err}")),
    };
    let arr = match value {
        serde_json::Value::Array(a) => a,
        other => return Err(format!("expected top-level JSON array, got {}", type_name(&other))),
    };
    if arr.is_empty() {
        return Err("expected at least one output paragraph, got empty array".into());
    }
    let mut out: Vec<LlmParagraph> = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        let obj = match item.as_object() {
            Some(o) => o,
            None => return Err(format!("output[{i}] is not an object")),
        };
        let end_clip = match obj.get("end_clip").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return Err(format!("output[{i}].end_clip missing or empty")),
        };
        let text = match obj.get("text").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return Err(format!("output[{i}].text missing")),
        };
        out.push(LlmParagraph { end_clip, text });
    }

    // Every input clip id in the hot zone + new material appears in
    // the input order. `end_clip` values must appear in that same
    // order and each must be a known id.
    let ordered_ids: Vec<String> = snapshot
        .hot
        .iter()
        .flat_map(|p| p.clips.iter().map(|c| c.id.clone()))
        .chain(snapshot.new_material.iter().map(|c| c.id.clone()))
        .collect();
    let known: HashSet<&str> = ordered_ids.iter().map(|s| s.as_str()).collect();
    let mut cursor = 0usize;
    for (i, p) in out.iter().enumerate() {
        if !known.contains(p.end_clip.as_str()) {
            return Err(format!(
                "output[{i}].end_clip={:?} is not a known input clip id",
                p.end_clip
            ));
        }
        // The end_clip must appear at or after the cursor position in
        // ordered_ids. Advance the cursor past it.
        let pos = match ordered_ids[cursor..].iter().position(|id| id == &p.end_clip) {
            Some(p) => cursor + p,
            None => {
                return Err(format!(
                    "output[{i}].end_clip={:?} is out of order (already consumed)",
                    p.end_clip
                ));
            }
        };
        cursor = pos + 1;
    }
    // The final output paragraph's end_clip must be the last input id
    // (otherwise clips are unassigned).
    if let Some(last_input) = ordered_ids.last() {
        if out.last().map(|p| p.end_clip.as_str()) != Some(last_input.as_str()) {
            return Err(format!(
                "final output.end_clip does not cover the last input clip ({:?})",
                last_input
            ));
        }
    }
    Ok(out)
}

fn type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Build the diffed paragraph vector from the validated LLM output and
/// apply it back onto the record state via `apply_llm_diff`.
///
/// Diff apply rules:
///   * `output.len() == hot.len()` — pure edit: keep existing hot ids,
///     update text + clip lists. Hardened flags unchanged.
///   * `output.len() > hot.len()` — freeze all-but-the-last-HOT_ZONE_N
///     of the response; the last N are the new hot zone.
///   * `output.len() < hot.len()` — hot zone shrinks; nothing new
///     hardens (the LLM merged paragraphs together).
///
/// Preserves paragraph `id` for the first N of the response so SSE
/// upserts are stable. Missing-from-response ids get broadcast as
/// `ParagraphRemove`.
fn apply_diff(
    record: &RecordState,
    slot: usize,
    snapshot: &ChannelSnapshot,
    output: Vec<LlmParagraph>,
) {
    // Walk the input clip list and cut at each `end_clip` to determine
    // which clips belong to each output paragraph.
    let all_clips: Vec<ClipRef> = snapshot
        .hot
        .iter()
        .flat_map(|p| p.clips.iter().cloned())
        .collect();
    // Map from clip id -> which output paragraph it belongs to.
    let mut cut_indices: Vec<usize> = Vec::with_capacity(output.len());
    {
        let mut cur = 0usize;
        for p in &output {
            match all_clips[cur..].iter().position(|c| c.id == p.end_clip) {
                Some(rel) => {
                    let abs = cur + rel;
                    cut_indices.push(abs);
                    cur = abs + 1;
                }
                None => {
                    // Shouldn't happen — parse_and_validate already
                    // rejected unknown ids. Abort cleanly.
                    warn!(slot, "paragraph: cut index missing for end_clip; aborting diff");
                    return;
                }
            }
        }
    }

    // Build the new paragraph list.
    let old_hot_len = snapshot.hot.len();
    let new_len = output.len();
    let mut new_tail: Vec<Paragraph> = Vec::with_capacity(new_len);
    let mut prev_cut: Option<usize> = None;
    for (i, out_p) in output.iter().enumerate() {
        let cut = cut_indices[i];
        let start = prev_cut.map(|c| c + 1).unwrap_or(0);
        let clips_range = &all_clips[start..=cut];
        let clips_vec: Vec<ClipRef> = clips_range.to_vec();
        let start_wall_ms = clips_vec.first().map(|c| c.start_wall_ms).unwrap_or(0);
        let end_wall_ms = clips_vec
            .last()
            .map(|c| {
                c.audio_duration_ms
                    .map(|d| c.start_wall_ms.saturating_add(d))
                    .unwrap_or(c.start_wall_ms)
            })
            .unwrap_or(0);
        // Preserve the id of the corresponding old hot paragraph for
        // the first `min(new_len, old_hot_len)` positions so SSE
        // subscribers see stable ids.
        let id = if i < old_hot_len.min(new_len) {
            snapshot.hot[i].id.clone()
        } else {
            Uuid::new_v4().to_string()
        };
        // Speaker + channel + created_at snapshot from the position's
        // old paragraph if available; otherwise from the first hot
        // paragraph (all clips share a channel).
        let template = snapshot
            .hot
            .get(i)
            .or_else(|| snapshot.hot.first())
            .cloned();
        let (channel, speaker, created_at) = if let Some(t) = template {
            (t.channel, t.speaker, t.created_at)
        } else {
            ("".to_string(), None, 0)
        };
        // Hardened iff this paragraph will scroll out of the new hot
        // zone. In the "grew" case, all but the last HOT_ZONE_N harden.
        let hardened = if new_len > HOT_ZONE_N {
            i + HOT_ZONE_N < new_len
        } else {
            // If the LLM re-emitted the whole hot zone, keep hardened=false
            // — those paragraphs stay in the editable window.
            false
        };
        let raw_text = clips_vec
            .iter()
            .map(|c| c.text.as_str())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        new_tail.push(Paragraph {
            id,
            channel,
            speaker,
            hardened,
            text: out_p.text.clone(),
            raw_text,
            clips: clips_vec,
            start_wall_ms,
            end_wall_ms,
            created_at,
        });
        prev_cut = Some(cut);
    }

    // Any paragraph in the previous hot zone whose id doesn't appear
    // in `new_tail` → treat as retired (merged into a neighbor).
    let new_ids: HashSet<&str> = new_tail.iter().map(|p| p.id.as_str()).collect();
    let removed_ids: Vec<String> = snapshot
        .hot
        .iter()
        .filter(|p| !new_ids.contains(p.id.as_str()))
        .map(|p| p.id.clone())
        .collect();

    let ok = record.apply_llm_diff(slot, snapshot.hot_start_index, new_tail, removed_ids);
    if !ok {
        warn!(slot, "paragraph: apply_llm_diff rejected (slot out of range)");
    } else {
        debug!(
            slot,
            hardened_count = snapshot.hardened_count,
            old_hot = old_hot_len,
            new_hot = new_len,
            "paragraph: applied LLM diff"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::ClipRef;

    fn mk_paragraph(id: &str, hardened: bool, clip_texts: &[&str]) -> Paragraph {
        let clips: Vec<ClipRef> = clip_texts
            .iter()
            .enumerate()
            .map(|(i, t)| ClipRef {
                id: format!("{id}-c{i}"),
                audio_start_ms: Some((i as u64) * 1000),
                audio_duration_ms: Some(1000),
                start_wall_ms: (i as u64) * 1000,
                text: t.to_string(),
                provisional: false,
                audio_url: None,
                mixed_start_ms: None,
            })
            .collect();
        let start = clips.first().map(|c| c.start_wall_ms).unwrap_or(0);
        let end = clips
            .last()
            .map(|c| c.start_wall_ms + c.audio_duration_ms.unwrap_or(0))
            .unwrap_or(0);
        Paragraph {
            id: id.to_string(),
            channel: "Vox 1".into(),
            speaker: None,
            hardened,
            text: clip_texts.join(" "),
            raw_text: clip_texts.join(" "),
            clips,
            start_wall_ms: start,
            end_wall_ms: end,
            created_at: 0,
        }
    }

    #[test]
    fn snapshot_splits_hardened_and_hot() {
        let paragraphs = vec![
            mk_paragraph("h1", true, &["hardened one"]),
            mk_paragraph("h2", true, &["hardened two"]),
            mk_paragraph("p1", false, &["hot one"]),
            mk_paragraph("p2", false, &["hot two"]),
        ];
        let snap = build_snapshot(&paragraphs);
        assert_eq!(snap.hardened_count, 2);
        assert_eq!(snap.hot_start_index, 2);
        assert_eq!(snap.hot.len(), 2);
        assert_eq!(snap.hot[0].id, "p1");
        assert_eq!(snap.hot[1].id, "p2");
        assert_eq!(snap.context.len(), 2);
        assert_eq!(snap.context[0].id, "h1");
    }

    #[test]
    fn snapshot_hot_capped_at_n() {
        let paragraphs = vec![
            mk_paragraph("p1", false, &["a"]),
            mk_paragraph("p2", false, &["b"]),
            mk_paragraph("p3", false, &["c"]),
            mk_paragraph("p4", false, &["d"]),
            mk_paragraph("p5", false, &["e"]),
        ];
        let snap = build_snapshot(&paragraphs);
        assert_eq!(snap.hot.len(), HOT_ZONE_N);
        // Newest last.
        assert_eq!(snap.hot[HOT_ZONE_N - 1].id, "p5");
    }

    #[test]
    fn parse_and_validate_rejects_unknown_id() {
        let paragraphs = vec![mk_paragraph("p1", false, &["hello world"])];
        let snap = build_snapshot(&paragraphs);
        let bad = serde_json::json!([{"end_clip": "does-not-exist", "text": "x"}]);
        let err = parse_and_validate(Ok(bad), &snap).unwrap_err();
        assert!(err.contains("not a known input clip id"), "got: {err}");
    }

    #[test]
    fn parse_and_validate_accepts_valid_output() {
        let paragraphs = vec![mk_paragraph("p1", false, &["hello", "world"])];
        let snap = build_snapshot(&paragraphs);
        let ok = serde_json::json!([
            {"end_clip": "p1-c1", "text": "hello world"}
        ]);
        let parsed = parse_and_validate(Ok(ok), &snap).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].end_clip, "p1-c1");
    }

    #[test]
    fn parse_and_validate_rejects_missing_last_clip_coverage() {
        // Two clips input but only the first is covered → last clip
        // would be unassigned. Must fail.
        let paragraphs = vec![mk_paragraph("p1", false, &["hello", "world"])];
        let snap = build_snapshot(&paragraphs);
        let missing_last = serde_json::json!([
            {"end_clip": "p1-c0", "text": "hello"}
        ]);
        let err = parse_and_validate(Ok(missing_last), &snap).unwrap_err();
        assert!(err.contains("does not cover the last input clip"), "got: {err}");
    }
}
