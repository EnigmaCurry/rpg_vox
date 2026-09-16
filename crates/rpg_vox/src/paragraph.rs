//! Pass-4 LLM per-clip conservative corrector.
//!
//! The Live-transcription pipeline emits provisional clips (pass 1),
//! finalized clips (pass 2), and boundary-smoothed clips (pass 3) into
//! [`crate::record::Paragraph`] containers on each per-channel state.
//!
//! Pass 4 (this module) takes each finalized-and-boundary-smoothed clip
//! ONE AT A TIME and sends its text to the LLM together with the active
//! project's dictionary vocabulary. The LLM returns a JSON array of
//! `{from, to}` edits — spelling / transcription / terminology fixes,
//! plus permission to rewrite or merge sentences within the clip when
//! they don't flow. Edits get applied as sequential substring
//! replacements on the clip's text, then the clip is marked
//! `pass4_ran=true` so future triggers skip it. Cross-clip edits are
//! out of scope by design; paragraph structure (splits/merges) is
//! owned by pass 3 soft-caps and the `PARAGRAPH_GAP_MS` timeout, not
//! by pass 4.
//!
//! Scheduling shape: one long-lived `tokio` task per Vox channel. All
//! calls on that channel are automatically serialized (single consumer
//! per mpsc receiver). TTS clips never fire the scheduler — their text
//! is already LLM-authored.

use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::chat::{self, ChatMessage};
use crate::record::{Paragraph, RecordState};
use crate::store::Store;

/// Master switch for pass 4. `false` short-circuits `run_once` before
/// any LLM work and tells the hardening logic in `record.rs` not to
/// wait for `pass4_ran`. Flip to `true` to re-enable the full LLM
/// proofreader. Kept as a const (not a runtime setting) so both sides
/// of the pipeline compile against the same truth.
pub const PASS4_ENABLED: bool = false;

/// Minimum wall-clock spacing between LLM calls on the same channel.
/// The scheduler task waits out the remainder of this window if a
/// trigger fires sooner. `ForcedBreak` honors the debounce too — the
/// spec says debounce paces timing, not whether the call happens.
pub const LLM_DEBOUNCE_MS: u64 = 3_000;

/// Per-call token cap for pass-4 responses. The LLM emits a JSON array
/// of `{from, to}` edit objects; typical clips have zero to a handful
/// of edits, but leaving generous headroom means truncated responses
/// (finish_reason=length, mid-string JSON) don't kill the cycle.
pub const LLM_MAX_TOKENS: u32 = 6_144;

/// One trigger fired by `finalize_clip` (or by a silence-gap paragraph
/// close) at the per-channel scheduler task. `Wake` says "consider
/// firing"; `ForcedBreak` says "consider firing soon regardless of the
/// debounce ceiling" (a silence-closed paragraph is a coherent moment
/// worth polishing at).
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
    store: Store,
    channel_count: usize,
) -> LlmScheduler {
    let mut senders: Vec<Option<mpsc::Sender<LlmTrigger>>> =
        Vec::with_capacity(channel_count + 1);
    for slot in 0..channel_count {
        let (tx, rx) = mpsc::channel::<LlmTrigger>(8);
        let record = record.clone();
        let chat = chat.clone();
        let store = store.clone();
        tokio::spawn(run_channel_task(record, chat, store, slot, rx));
        senders.push(Some(tx));
    }
    // TTS pseudo-channel entry — no task, no sender. Slot is reserved
    // so a caller indexing by ChannelKind::Tts semantics stays sane.
    senders.push(None);
    LlmScheduler { senders }
}

/// One channel's LLM scheduler loop. Reads triggers, applies debounce,
/// runs the LLM call, applies the resulting edit list. Never returns
/// errors — logs on failure and continues.
async fn run_channel_task(
    record: RecordState,
    chat: chat::Client,
    store: Store,
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
        let _ = forced; // per-clip mode doesn't distinguish forced today.
        info!(slot, "pass4: trigger received, entering run_once");
        match run_once(&record, &chat, &store, slot).await {
            LlmOutcome::Fired => {
                last_llm_at = Some(Instant::now());
            }
            LlmOutcome::Skipped(reason) => {
                info!(slot, reason, "pass4: skipped LLM cycle");
            }
            LlmOutcome::Aborted(reason) => {
                warn!(slot, reason, "pass4: aborted LLM cycle");
            }
        }
        // Drain any additional triggers that arrived DURING the call.
        while let Ok(_t) = rx.try_recv() {}
    }
}

enum LlmOutcome {
    Fired,
    Skipped(&'static str),
    Aborted(&'static str),
}

/// One decision cycle. Finds the newest clip on `slot` that has been
/// pass-3-smoothed but not yet pass-4-polished, sends it to the LLM
/// with the active project's vocabulary, applies the resulting edit
/// list to the clip's text.
async fn run_once(
    record: &RecordState,
    chat: &chat::Client,
    store: &Store,
    slot: usize,
) -> LlmOutcome {
    // Master switch. `record.rs`'s hardening logic keys off the same
    // const so paragraphs harden on the normal 2 s timeout while
    // pass 4 is off — no 30 s wait for LLM output that will never
    // arrive.
    if !PASS4_ENABLED {
        let _ = (chat, store, slot);
        return LlmOutcome::Skipped("pass 4 disabled (PASS4_ENABLED=false)");
    }
    // Live-buffer gate: while no named recording is active, only run
    // when the operator has explicitly opted in via the "Reinterpret
    // with LLM" checkbox on the live pane. Default is off — the live
    // buffer stays as a plain per-clip transcript otherwise. When a
    // recording is active, LLM cycles run regardless of this flag.
    if !record.is_recording() && !record.llm_when_idle() {
        return LlmOutcome::Skipped("live buffer, llm disabled");
    }

    // Snapshot channel state under the state lock, then drop it before
    // any async work. We pull ALL paragraphs so we can walk newest-first
    // looking for a pass-4 candidate.
    let Some(paragraphs) = record.snapshot_channel_paragraphs(slot) else {
        return LlmOutcome::Skipped("slot out of range");
    };

    let Some(target) = find_next_pass4_target(&paragraphs) else {
        return LlmOutcome::Skipped("no eligible clip");
    };
    let PassFourTarget {
        paragraph_id,
        clip_id,
        clip_text,
    } = target;

    // Load the active project's vocabulary. `None` = no project selected
    // yet; empty vec = project has no dictionary entries. Both cases
    // just mean "empty vocab list" in the prompt.
    let vocab = load_active_project_vocab(store).await;

    let user_payload = clip_text.clone();
    let history = vec![ChatMessage {
        role: "user".into(),
        content: user_payload.clone(),
    }];
    let system_prompt = build_system_prompt(&vocab);
    let schema = response_schema();

    info!(
        slot,
        paragraph = %paragraph_id,
        clip = %clip_id,
        vocab_words = vocab.len(),
        clip_chars = clip_text.len(),
        "pass4: sending clip to LLM"
    );
    info!(slot, clip = %clip_id, text = %clip_text, "pass4: input clip text");

    // Flip the per-channel inflight flag on so the snapshot's
    // `channel.llm_inflight` reads true for the UI's "reorganizing…"
    // pulse. Guard drops it back to false on any exit path.
    let _inflight_guard = LlmInflightGuard::new(record.clone(), slot);

    let attempt = chat
        .generate_reply_json(
            history.clone(),
            Some(system_prompt.clone()),
            schema.clone(),
            Some(LLM_MAX_TOKENS),
        )
        .await;
    log_llm_response(slot, "first", &attempt);
    let edits = match parse_edits(attempt) {
        Ok(edits) => edits,
        Err(reason) => {
            // Retry once with the failure reason appended.
            info!(slot, reason = %reason, "pass4: first attempt rejected — retrying");
            let addendum = format!(
                "{system_prompt}\n\nYour previous response was rejected: {reason}. \
                 Return a valid JSON array matching the schema."
            );
            let retry = chat
                .generate_reply_json(
                    history,
                    Some(addendum),
                    response_schema(),
                    Some(LLM_MAX_TOKENS),
                )
                .await;
            log_llm_response(slot, "retry", &retry);
            match parse_edits(retry) {
                Ok(edits) => edits,
                Err(final_reason) => {
                    warn!(slot, reason = %final_reason, "pass4: LLM cycle rejected twice");
                    // Mark the clip pass4_ran anyway so we don't retry
                    // forever on a clip the LLM won't parse. Preserves
                    // original text.
                    record.apply_pass4_clip_text(&clip_id, clip_text);
                    return LlmOutcome::Aborted("LLM output invalid after retry");
                }
            }
        }
    };

    let new_text = apply_edits(&clip_text, &edits);
    info!(
        slot,
        clip = %clip_id,
        edits = edits.len(),
        old = %clip_text,
        new = %new_text,
        "pass4: applied edits"
    );

    record.apply_pass4_clip_text(&clip_id, new_text);
    LlmOutcome::Fired
}

/// The clip pass 4 will polish this cycle.
struct PassFourTarget {
    paragraph_id: String,
    clip_id: String,
    clip_text: String,
}

/// Walk the channel's paragraph list newest-first and return the first
/// clip that is:
///   - not provisional (pass-2 finalize has landed);
///   - !pass4_ran (we haven't polished it yet);
///   - not empty text.
///
/// Deliberately does NOT require `pass3_ran`. Pass 3 legitimately
/// discards its own attempts when the joined re-decode fails a sanity
/// check (edge-word mismatch, per-clip shrinkage) — those discards
/// leave the clip's per-clip decode as authoritative and never flip
/// `pass3_ran=true`. Gating pass 4 on `pass3_ran` would starve every
/// such clip forever. Race with a later successful pass 3: pass 3's
/// apply overwrites pass 4's text (pass 3 doesn't check `pass4_ran`),
/// so pass 3 wins by write-order — tolerable, both agree the
/// transcript should be readable, they just disagree on liberties.
fn find_next_pass4_target(paragraphs: &[Paragraph]) -> Option<PassFourTarget> {
    for paragraph in paragraphs.iter().rev() {
        for clip in paragraph.clips.iter().rev() {
            if clip.provisional || clip.pass4_ran {
                continue;
            }
            let text = clip.text.trim();
            if text.is_empty() {
                continue;
            }
            return Some(PassFourTarget {
                paragraph_id: paragraph.id.clone(),
                clip_id: clip.id.clone(),
                clip_text: text.to_string(),
            });
        }
    }
    None
}

/// Load the active project's dictionary words, if any. Reads the
/// currently-open project id from the store, then that project's
/// dictionary from `app_state`. Any of "no project selected", "project
/// missing", or "empty dictionary" returns an empty vec — the LLM just
/// gets an empty vocab section in the prompt.
async fn load_active_project_vocab(store: &Store) -> Vec<String> {
    let Ok(Some(project_id)) = store.get_active_project().await else {
        return Vec::new();
    };
    let Ok(Some(raw)) = store.get_app_state().await else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    let Some(projects) = value.get("projects").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let Some(project) = projects
        .iter()
        .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(project_id.as_str()))
    else {
        return Vec::new();
    };
    let Some(entries) = project.get("dictionary").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|e| {
            let word = e.get("word").and_then(|v| v.as_str())?.trim();
            if word.is_empty() { None } else { Some(word.to_string()) }
        })
        .collect()
}

/// Build the system prompt with the vocabulary list appended.
fn build_system_prompt(vocab: &[String]) -> String {
    let vocab_section = if vocab.is_empty() {
        "(none)".to_string()
    } else {
        vocab.join("\n")
    };
    format!("{PROMPT_BASE}\n\nKnown vocabulary:\n\n{vocab_section}")
}

const PROMPT_BASE: &str = "You are a conservative text-correction engine.\n\
\n\
Your job is to identify spelling, transcription, and terminology errors in the provided text.\n\
\n\
Do NOT rewrite, paraphrase, rephrase, improve style, or change grammar unless required to correct an obvious error. The one exception: if the input has awkward sentence breaks or run-ons that make the passage hard to read, you MAY split or merge sentences WITHIN the provided text so the result flows naturally. Do not add or remove content, only re-punctuate and re-capitalize as needed for readability.\n\
\n\
You may use the supplied vocabulary to recognize project-specific identifiers and terminology, including fuzzy or phonetic matches.\n\
\n\
Return ONLY a JSON array of edits.\n\
\n\
Each edit must have this form:\n\
\n\
{\n\
\"from\": \"exact text copied from the input\",\n\
\"to\": \"replacement text\"\n\
}\n\
\n\
Rules:\n\
\n\
    \"from\" MUST be an exact substring of the original input.\n\
\n\
    Include enough surrounding words in \"from\" to make the match unambiguous when necessary.\n\
\n\
    \"to\" should contain only the corrected replacement.\n\
\n\
    Preserve capitalization unless correcting it is necessary.\n\
\n\
    Preserve punctuation unless correcting it is necessary.\n\
\n\
    Prefer vocabulary entries when the input closely resembles them.\n\
\n\
    Do not make speculative corrections.\n\
\n\
    Do not return unchanged text.\n\
\n\
    If there are no corrections, return [].\n\
\n\
    Return valid JSON only. Do not use Markdown code fences.\n\
\n\
    Do not include explanations, comments, confidence scores, or any other text.";

/// JSON schema handed to the LLM via `response_format` — array of
/// `{from, to}` string pairs. Kept as a `serde_json::Value` so
/// llama.cpp's grammar-constrained decoding sees the exact structure
/// we validate against.
fn response_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "from": {"type": "string"},
                "to": {"type": "string"}
            },
            "required": ["from", "to"],
            "additionalProperties": false
        }
    })
}

/// One edit returned by the LLM. `from` must be an exact substring of
/// the input clip text; `to` is the replacement.
#[derive(Debug, Clone)]
struct Edit {
    from: String,
    to: String,
}

/// Parse the LLM response into a list of edits. Returns a short reason
/// string on parse / shape failure so the retry path can surface it in
/// the addendum.
fn parse_edits(res: anyhow::Result<serde_json::Value>) -> Result<Vec<Edit>, String> {
    let value = res.map_err(|err| format!("transport error: {err:#}"))?;
    let arr = value
        .as_array()
        .ok_or_else(|| "response is not a JSON array".to_string())?;
    let mut out = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        let obj = item
            .as_object()
            .ok_or_else(|| format!("edit at index {i} is not an object"))?;
        let from = obj
            .get("from")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("edit at index {i} missing string `from`"))?;
        let to = obj
            .get("to")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("edit at index {i} missing string `to`"))?;
        if from.is_empty() {
            return Err(format!("edit at index {i} has empty `from`"));
        }
        out.push(Edit {
            from: from.to_string(),
            to: to.to_string(),
        });
    }
    Ok(out)
}

/// Apply an edit list to a text string. Each edit's `from` is replaced
/// (first occurrence only) with its `to`. Edits whose `from` is not
/// found in the current working text are silently skipped — the LLM
/// occasionally hallucinates edit targets and we don't want a stale
/// edit to corrupt an already-applied later edit. Applied in the order
/// the LLM emitted them.
fn apply_edits(text: &str, edits: &[Edit]) -> String {
    let mut out = text.to_string();
    for edit in edits {
        if let Some(pos) = out.find(&edit.from) {
            let end = pos + edit.from.len();
            let mut next = String::with_capacity(out.len() + edit.to.len());
            next.push_str(&out[..pos]);
            next.push_str(&edit.to);
            next.push_str(&out[end..]);
            out = next;
        }
    }
    out
}

/// Log the raw LLM response (or the transport error) so operators can
/// diff request vs response when the model is misbehaving.
fn log_llm_response(slot: usize, attempt: &str, res: &anyhow::Result<serde_json::Value>) {
    match res {
        Ok(v) => {
            let raw = serde_json::to_string(v).unwrap_or_else(|_| "<unserializable>".into());
            info!(
                slot,
                attempt,
                bytes = raw.len(),
                raw = %raw,
                "pass4 LLM response"
            );
        }
        Err(err) => {
            info!(
                slot,
                attempt,
                err = %format!("{err:#}"),
                "pass4 LLM response error"
            );
        }
    }
}

/// Scope guard that flips a channel's `llm_inflight` flag on
/// construction and off on drop. Ensures the snapshot's
/// "reorganizing…" indicator clears on every exit path from
/// `run_once`, including panics.
struct LlmInflightGuard {
    record: RecordState,
    slot: usize,
}

impl LlmInflightGuard {
    fn new(record: RecordState, slot: usize) -> Self {
        record.set_llm_inflight(slot, true);
        Self { record, slot }
    }
}

impl Drop for LlmInflightGuard {
    fn drop(&mut self) {
        self.record.set_llm_inflight(self.slot, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_edits_replaces_first_occurrence() {
        let edits = vec![Edit {
            from: "PipeWire".into(),
            to: "PipeWire".into(),
        }];
        // "pipe wire" is not an exact match — nothing changes.
        assert_eq!(apply_edits("pipe wire", &edits), "pipe wire");
    }

    #[test]
    fn apply_edits_sequential() {
        let edits = vec![
            Edit { from: "quen three".into(), to: "Qwen3".into() },
            Edit { from: "TTS".into(), to: "TTS".into() },
        ];
        let text = "the quen three TTS model";
        assert_eq!(apply_edits(text, &edits), "the Qwen3 TTS model");
    }

    #[test]
    fn apply_edits_skips_missing_from() {
        let edits = vec![
            Edit { from: "nonexistent".into(), to: "X".into() },
            Edit { from: "hello".into(), to: "HELLO".into() },
        ];
        assert_eq!(apply_edits("hello world", &edits), "HELLO world");
    }

    #[test]
    fn parse_edits_accepts_empty_array() {
        let v = serde_json::json!([]);
        let edits = parse_edits(Ok(v)).unwrap();
        assert!(edits.is_empty());
    }

    #[test]
    fn parse_edits_accepts_well_formed() {
        let v = serde_json::json!([
            {"from": "foo", "to": "bar"},
            {"from": "baz", "to": "qux"},
        ]);
        let edits = parse_edits(Ok(v)).unwrap();
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].from, "foo");
        assert_eq!(edits[1].to, "qux");
    }

    #[test]
    fn parse_edits_rejects_non_array() {
        let v = serde_json::json!({"from": "foo", "to": "bar"});
        assert!(parse_edits(Ok(v)).is_err());
    }

    #[test]
    fn parse_edits_rejects_missing_from() {
        let v = serde_json::json!([{"to": "bar"}]);
        assert!(parse_edits(Ok(v)).is_err());
    }
}
