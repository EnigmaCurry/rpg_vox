//! Word timings. Offline recognizers report per-token timestamps; these
//! are grouped into words, then kept attached to the text as later
//! passes rewrite it. Text and timings are matched word by word with an
//! LCS, so a word that survives a rewrite keeps its time and a new or
//! changed word is spread across the gap its neighbours leave.

use serde::{Deserialize, Serialize};

/// One word with its audio span, in ms of audio (absolute on clips and
/// paragraphs, relative to the buffer start straight out of a recognizer).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

/// Longest a word without a reported duration is assumed to last.
const MAX_GUESSED_WORD_MS: u64 = 800;

/// Group recognizer tokens into words. A token that begins with
/// whitespace starts a new word. `durations` may be empty (SenseVoice),
/// in which case a word ends where the next one starts, capped at
/// [`MAX_GUESSED_WORD_MS`] and at `total_ms`.
pub fn words_from_tokens(
    tokens: &[String],
    timestamps: &[f32],
    durations: &[f32],
    total_ms: u64,
) -> Vec<Word> {
    let ms = |s: f32| (s.max(0.0) * 1000.0).round() as u64;
    let mut words: Vec<Word> = Vec::new();
    let mut open_end: Option<u64> = None;
    for (i, tok) in tokens.iter().enumerate() {
        let Some(&ts) = timestamps.get(i) else { break };
        let start = ms(ts).min(total_ms);
        let end = durations.get(i).map(|&d| ms(ts + d).min(total_ms));
        let piece = tok.trim();
        let new_word = words.is_empty() || tok.starts_with(char::is_whitespace);
        if new_word {
            // A bare " " token opens the word its following tokens fill.
            words.push(Word {
                text: piece.to_string(),
                start_ms: start,
                end_ms: end.unwrap_or(start),
            });
        } else if let Some(w) = words.last_mut() {
            w.text.push_str(piece);
            if let Some(e) = end {
                w.end_ms = w.end_ms.max(e);
            }
        }
        open_end = end;
    }
    words.retain(|w| !w.text.is_empty());
    if durations.is_empty() || open_end.is_none() {
        for i in 0..words.len() {
            let next = words.get(i + 1).map(|n| n.start_ms).unwrap_or(total_ms);
            let w = &mut words[i];
            w.end_ms = next.min(w.start_ms + MAX_GUESSED_WORD_MS).max(w.start_ms);
        }
    }
    words
}

fn norm(w: &str) -> String {
    w.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Timings for the words of `text`, taken from `old` wherever a word
/// matches (case and punctuation ignored) and interpolated by character
/// length in between. `span` bounds words that have no matched
/// neighbour on one side. The result has one entry per whitespace word
/// of `text`, with that word's exact spelling.
pub fn align_words(old: &[Word], text: &str, span: (u64, u64)) -> Vec<Word> {
    let new: Vec<&str> = text.split_whitespace().collect();
    let a: Vec<String> = old.iter().map(|w| norm(&w.text)).collect();
    let b: Vec<String> = new.iter().map(|w| norm(w)).collect();

    // matched[j] = Some(i) when new word j is old word i.
    let mut matched: Vec<Option<usize>> = vec![None; new.len()];
    let mut t = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            t[i][j] = if !a[i].is_empty() && a[i] == b[j] {
                t[i + 1][j + 1] + 1
            } else {
                t[i + 1][j].max(t[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if !a[i].is_empty() && a[i] == b[j] {
            matched[j] = Some(i);
            i += 1;
            j += 1;
        } else if t[i + 1][j] >= t[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }

    let (lo, hi) = (span.0, span.1.max(span.0));
    let mut out: Vec<Word> = Vec::with_capacity(new.len());
    let mut j = 0;
    while j < new.len() {
        if let Some(i) = matched[j] {
            out.push(Word {
                text: new[j].to_string(),
                start_ms: old[i].start_ms,
                end_ms: old[i].end_ms,
            });
            j += 1;
            continue;
        }
        // A run of unmatched words takes the span of the old words it
        // replaces, or the gap between its matched neighbours when it is
        // a pure insertion.
        let run_end = (j..new.len())
            .find(|&k| matched[k].is_some())
            .unwrap_or(new.len());
        let prev_old = j.checked_sub(1).and_then(|p| matched[p]);
        let next_old = matched.get(run_end).copied().flatten();
        let first_replaced = prev_old.map(|p| p + 1).unwrap_or(0);
        let past_replaced = next_old.unwrap_or(old.len());
        let (from, to) = if first_replaced < past_replaced {
            (old[first_replaced].start_ms, old[past_replaced - 1].end_ms)
        } else {
            (
                prev_old.map(|p| old[p].end_ms).unwrap_or(lo),
                next_old.map(|n| old[n].start_ms).unwrap_or(hi),
            )
        };
        let to = to.max(from);
        let lens: Vec<u64> = new[j..run_end]
            .iter()
            .map(|w| w.chars().count().max(1) as u64)
            .collect();
        let total: u64 = lens.iter().sum();
        let mut acc = 0;
        for (k, len) in lens.iter().enumerate() {
            let s = from + (to - from) * acc / total;
            acc += len;
            let e = from + (to - from) * acc / total;
            out.push(Word {
                text: new[j + k].to_string(),
                start_ms: s,
                end_ms: e,
            });
        }
        j = run_end;
    }
    out
}

/// Shift every word by `offset_ms`.
pub fn offset(words: &mut [Word], offset_ms: u64) {
    for w in words {
        w.start_ms += offset_ms;
        w.end_ms += offset_ms;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, s: u64, e: u64) -> Word {
        Word {
            text: text.into(),
            start_ms: s,
            end_ms: e,
        }
    }

    fn toks(t: &[&str]) -> Vec<String> {
        t.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn groups_parakeet_tokens() {
        let tokens = toks(&[" The", " G", "ob", " ", "2", "0", ","]);
        let ts = [0.0, 0.16, 0.24, 1.68, 1.84, 1.84, 2.08];
        let du = [0.16, 0.08, 0.08, 0.16, 0.0, 0.08, 0.24];
        let words = words_from_tokens(&tokens, &ts, &du, 5000);
        assert_eq!(
            words,
            vec![w("The", 0, 160), w("Gob", 160, 320), w("20,", 1680, 2320)]
        );
    }

    #[test]
    fn guesses_ends_without_durations() {
        let tokens = toks(&["The", " King", " rolled"]);
        let words = words_from_tokens(&tokens, &[0.06, 0.6, 2.5], &[], 3000);
        assert_eq!(
            words,
            vec![
                w("The", 60, 600),
                w("King", 600, 1400),
                w("rolled", 2500, 3000)
            ]
        );
    }

    #[test]
    fn keeps_matched_times_and_interpolates_edits() {
        let old = vec![
            w("at", 0, 100),
            w("11.42", 200, 500),
            w("in", 600, 700),
            w("Graftton.", 800, 1400),
        ];
        let out = align_words(&old, "at 11:42 PM in Grafton.", (0, 1500));
        assert_eq!(out[0], w("at", 0, 100));
        // "11.42" and "11:42" normalise to the same word.
        assert_eq!(out[1], w("11:42", 200, 500));
        // Pure insertion: the gap between its neighbours.
        assert_eq!(out[2], w("PM", 500, 600));
        assert_eq!(out[3], w("in", 600, 700));
        // Replacement: the span of the word it replaced.
        assert_eq!(out[4], w("Grafton.", 800, 1400));
    }

    #[test]
    fn interpolates_without_anchors() {
        let out = align_words(&[], "aa bb", (1000, 2000));
        assert_eq!(out, vec![w("aa", 1000, 1500), w("bb", 1500, 2000)]);
        assert!(align_words(&[w("x", 0, 1)], "", (0, 1)).is_empty());
    }
}
