//! Pass 3: boundary re-transcription.
//!
//! Pass 2 decodes each VAD utterance on its own, so words that straddle
//! a cut get mangled and every clip ends with SenseVoice's reflexive
//! period. Pass 3 re-decodes the audio of the last 2–3 clips of a
//! paragraph as one buffer, then splits the joined text back across the
//! clips by word timestamps (or by duration ratio when the recognizer
//! reports none). Several guards reject decodes that would
//! lose content rather than improve it.
//!
//! Everything here is pure; the engine owns the audio ring, the
//! recognizer call and applying the result.

use crate::filters::{count_words, is_false_positive};
use crate::model::Paragraph;
use crate::timing::{align_words, offset, Word};

#[derive(Debug, Clone)]
pub struct BoundaryConfig {
    /// Most clips re-decoded together.
    pub max_clips: usize,
    /// Cap on the audio span of one window. SenseVoice degrades on
    /// inputs much longer than ~25 s.
    pub max_window_ms: u64,
    /// Longest silence across which a clip from the previous paragraph
    /// may be borrowed as context.
    pub borrow_max_gap_ms: u64,
}

impl Default for BoundaryConfig {
    fn default() -> Self {
        Self {
            max_clips: 3,
            max_window_ms: 25_000,
            borrow_max_gap_ms: 3_000,
        }
    }
}

/// One clip in a pass-3 window.
#[derive(Debug, Clone)]
pub struct WindowClip {
    pub clip_id: String,
    pub paragraph_id: String,
    pub start_ms: u64,
    pub duration_ms: u64,
    pub text: String,
    /// False for a clip borrowed from the previous paragraph: it is
    /// context audio only and its text is never rewritten.
    pub writable: bool,
}

fn window_span(w: &[WindowClip]) -> u64 {
    match (w.first(), w.last()) {
        (Some(first), Some(last)) => {
            (last.start_ms + last.duration_ms).saturating_sub(first.start_ms)
        }
        _ => 0,
    }
}

/// Pick the clips to re-decode for `paragraphs[idx]`. `None` when there
/// is not enough finalized context or the window can't fit the budget.
///
/// `paragraph_gap_ms` is the silence that opens a new paragraph; a
/// previous paragraph that has been quiet that long counts as hardened
/// and is never borrowed from.
pub fn select_window(
    paragraphs: &[Paragraph],
    idx: usize,
    now_ms: u64,
    paragraph_gap_ms: u64,
    cfg: &BoundaryConfig,
) -> Option<Vec<WindowClip>> {
    let paragraph = paragraphs.get(idx)?;
    let recent: Vec<_> = paragraph
        .clips
        .iter()
        .filter(|c| c.duration_ms.is_some())
        .rev()
        .take(cfg.max_clips)
        .collect();
    // A pass-2 decode is still landing in this window; its arrival
    // schedules pass 3 again.
    if recent.iter().any(|c| c.is_partial()) {
        return None;
    }
    let mut window: Vec<WindowClip> = recent
        .into_iter()
        .map(|c| WindowClip {
            clip_id: c.id.clone(),
            paragraph_id: paragraph.id.clone(),
            start_ms: c.start_ms,
            duration_ms: c.duration_ms.unwrap_or(0),
            text: c.text.clone(),
            writable: true,
        })
        .collect();
    window.reverse();

    // Cross-paragraph borrow: a paragraph with a single clip reaches back
    // for the previous paragraph's last clip as context, provided that
    // paragraph isn't hardened, the silence between them is short, and
    // the combined span fits the budget.
    if window.len() < 2 {
        if let Some(prev) = idx.checked_sub(1).and_then(|i| paragraphs.get(i)) {
            let prev_end = prev.clips.last().map(|c| c.end_ms()).unwrap_or(0);
            let prev_hardened =
                prev.hardened || now_ms.saturating_sub(prev_end) >= paragraph_gap_ms;
            let candidate = prev
                .clips
                .iter()
                .rev()
                .find(|c| c.duration_ms.is_some() && !c.is_partial());
            if let (false, Some(borrow), Some(cur)) = (prev_hardened, candidate, window.last()) {
                let gap = cur.start_ms.saturating_sub(borrow.end_ms());
                let span = (cur.start_ms + cur.duration_ms).saturating_sub(borrow.start_ms);
                if gap <= cfg.borrow_max_gap_ms && span <= cfg.max_window_ms {
                    window.insert(
                        0,
                        WindowClip {
                            clip_id: borrow.id.clone(),
                            paragraph_id: prev.id.clone(),
                            start_ms: borrow.start_ms,
                            duration_ms: borrow.duration_ms.unwrap_or(0),
                            text: borrow.text.clone(),
                            writable: false,
                        },
                    );
                }
            }
        }
    }
    if window.len() < 2 {
        return None;
    }
    // Trim oldest until the audio span (including the silence between
    // clips) fits, never below two clips.
    while window.len() > 2 && window_span(&window) > cfg.max_window_ms {
        window.remove(0);
    }
    if window_span(&window) > cfg.max_window_ms {
        return None;
    }
    if window.iter().map(|w| w.duration_ms).sum::<u64>() == 0 {
        return None;
    }
    Some(window)
}

/// Why a pass-3 decode was thrown away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    /// Decode is a known hallucination ("I.", a lone CJK glyph).
    FalsePositive,
    /// Decode has fewer than half the words already on screen.
    TooShort,
    /// First or last word differs from the clips' current edges, so the
    /// proportional split would shift words across clips.
    EdgeMismatch,
    /// Some writable clip would lose more than 30% of its words.
    Shrinkage,
}

/// New text and word timings for one window clip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipRevision {
    pub text: String,
    /// Absolute times; empty when the decode had no timings.
    pub words: Vec<Word>,
}

/// Validate a joined re-decode against the window and split it back
/// into one revision per window clip. `words` are the decode's word
/// timings relative to the window's first clip; when present the split
/// follows them, otherwise it is proportional to clip duration.
pub fn plan_revision(
    window: &[WindowClip],
    joined: &str,
    words: &[Word],
) -> Result<Vec<ClipRevision>, Reject> {
    let joined = joined.trim();
    let existing_words: usize = window.iter().map(|w| count_words(&w.text)).sum();
    let out_words = count_words(joined);
    if is_false_positive(joined) {
        return Err(Reject::FalsePositive);
    }
    if existing_words >= 6 && out_words < (existing_words as f64 * 0.5).round() as usize {
        return Err(Reject::TooShort);
    }

    let norm = |w: &str| {
        w.trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase()
    };
    let joined_first = joined.split_whitespace().next().map(norm);
    let joined_last = joined.split_whitespace().last().map(norm);
    let first = window
        .first()
        .and_then(|w| w.text.split_whitespace().next().map(norm));
    let last = window
        .last()
        .and_then(|w| w.text.split_whitespace().last().map(norm));
    if joined_first != first || joined_last != last {
        return Err(Reject::EdgeMismatch);
    }

    let split = if words.is_empty() {
        let durations: Vec<u64> = window.iter().map(|w| w.duration_ms).collect();
        split_boundary_text(joined, &durations)
            .into_iter()
            .map(|text| ClipRevision {
                text,
                words: Vec::new(),
            })
            .collect()
    } else {
        split_by_time(window, joined, words)
    };
    let shrinks = window
        .iter()
        .zip(split.iter())
        .filter(|(w, _)| w.writable)
        .any(|(w, new)| {
            let old_w = count_words(&w.text);
            old_w >= 3 && count_words(&new.text) < (old_w as f64 * 0.7).round() as usize
        });
    if shrinks {
        return Err(Reject::Shrinkage);
    }
    Ok(split)
}

/// Assign each word of `joined` to the window clip its midpoint falls in
/// (or the nearest one, if it lands in the silence between clips),
/// never going back to an earlier clip.
fn split_by_time(window: &[WindowClip], joined: &str, words: &[Word]) -> Vec<ClipRevision> {
    let base = window.first().map(|w| w.start_ms).unwrap_or(0);
    let mut timed = align_words(words, joined, (0, window_span(window)));
    offset(&mut timed, base);
    let mut out: Vec<ClipRevision> = window
        .iter()
        .map(|_| ClipRevision {
            text: String::new(),
            words: Vec::new(),
        })
        .collect();
    let mut at = 0;
    for w in timed {
        let mid = (w.start_ms + w.end_ms) / 2;
        let dist = |c: &WindowClip| {
            let end = c.start_ms + c.duration_ms;
            if mid < c.start_ms {
                c.start_ms - mid
            } else {
                mid.saturating_sub(end)
            }
        };
        while at + 1 < window.len() && dist(&window[at + 1]) < dist(&window[at]) {
            at += 1;
        }
        out[at].words.push(w);
    }
    for r in &mut out {
        r.text = r
            .words
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
    }
    out
}

/// Proportional split of `text` across clips by duration ratio. Cut
/// points snap to the nearest whitespace so words never split across
/// two clips. Returns one string per entry in `durations`.
pub fn split_boundary_text(text: &str, durations: &[u64]) -> Vec<String> {
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
        let mut out = vec![String::new(); n];
        out[n - 1] = text.to_string();
        return out;
    }
    // Char indices (not bytes) so multi-byte UTF-8 is safe; trailing
    // sentinel at `text.len()`.
    let char_positions: Vec<usize> = text
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()))
        .collect();
    let total_chars = char_positions.len() - 1;
    let half_word_width = ((total_chars as u64) / (n as u64 * 2)).max(1) as usize;
    let chars: Vec<char> = text.chars().collect();
    let snap_to_whitespace = |target: usize| -> usize {
        if target == 0 || target >= total_chars {
            return target.min(total_chars);
        }
        for d in 0..=half_word_width {
            let left = target.saturating_sub(d);
            let right = (target + d).min(total_chars);
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
        let slice = if i + 1 == n {
            text[char_positions[prev_char_end]..].trim().to_string()
        } else {
            acc = acc.saturating_add(*dur);
            let frac_end = ((acc as f64 / total as f64) * total_chars as f64).round() as usize;
            let char_end = snap_to_whitespace(frac_end.min(total_chars)).max(prev_char_end);
            let s = text[char_positions[prev_char_end]..char_positions[char_end]]
                .trim()
                .to_string();
            prev_char_end = char_end;
            s
        };
        out.push(slice);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wc(text: &str, start: u64, dur: u64, writable: bool) -> WindowClip {
        WindowClip {
            clip_id: text.into(),
            paragraph_id: "p".into(),
            start_ms: start,
            duration_ms: dur,
            text: text.into(),
            writable,
        }
    }

    #[test]
    fn split_is_proportional_and_word_aligned() {
        let out = split_boundary_text("one two three four", &[1000, 1000]);
        assert_eq!(out, vec!["one two", "three four"]);
        let out = split_boundary_text("héllo wörld ünïcode", &[1, 2]);
        assert_eq!(out.join(" "), "héllo wörld ünïcode");
    }

    #[test]
    fn split_handles_degenerate_inputs() {
        assert_eq!(split_boundary_text("", &[1, 1]), vec!["", ""]);
        assert_eq!(split_boundary_text("a b", &[0, 0]), vec!["", "a b"]);
        assert!(split_boundary_text("a", &[]).is_empty());
    }

    #[test]
    fn revision_accepts_boundary_fix() {
        let w = vec![
            wc("We went to the.", 0, 1500, true),
            wc("Store today.", 1600, 1000, true),
        ];
        let out = plan_revision(&w, "We went to the store today.", &[]).unwrap();
        assert_eq!(out.len(), 2);
        let texts: Vec<&str> = out.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts.join(" "), "We went to the store today.");
    }

    #[test]
    fn revision_splits_on_word_times() {
        let w = vec![
            wc("We went to the.", 1000, 1500, true),
            wc("Store today.", 2600, 1000, true),
        ];
        // "the" starts late in clip 1 but its midpoint is still there;
        // "store" falls in the silence nearer clip 2.
        let words = [
            ("We", 0, 200),
            ("went", 200, 500),
            ("to", 500, 700),
            ("the", 1300, 1490),
            ("store", 1560, 1900),
            ("today.", 1900, 2400),
        ]
        .map(|(t, s, e)| Word {
            text: t.into(),
            start_ms: s,
            end_ms: e,
        });
        let out = plan_revision(&w, "We went to the store today.", &words).unwrap();
        assert_eq!(out[0].text, "We went to the");
        assert_eq!(out[1].text, "store today.");
        assert_eq!(out[1].words[0].start_ms, 2560);
    }

    #[test]
    fn revision_rejects_lossy_decodes() {
        let w = vec![
            wc("one two three four.", 0, 2000, true),
            wc("five six seven eight.", 2100, 2000, true),
        ];
        assert_eq!(plan_revision(&w, "I.", &[]), Err(Reject::FalsePositive));
        assert_eq!(plan_revision(&w, "one eight.", &[]), Err(Reject::TooShort));
        assert_eq!(
            plan_revision(&w, "two three four five six seven eight.", &[]),
            Err(Reject::EdgeMismatch)
        );
    }
}
