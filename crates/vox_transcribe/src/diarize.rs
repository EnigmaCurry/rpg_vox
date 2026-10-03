//! Relabel a finished transcript from a full offline diarization: who
//! spoke when, over the whole recording. Each word goes to the speaker
//! whose turn covers it, paragraphs split where the speaker changes, and
//! speakers are lettered in order of first appearance, as the live labels
//! are, so the two agree wherever the live pass got it right.

use std::collections::HashMap;

use crate::model::{Clip, Paragraph, Stage, Transcript};
use crate::speaker::label;
use crate::timing::Word;

/// One speaker turn, in ms of audio. `speaker` is the diarizer's index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker: usize,
}

/// A word this far from every turn keeps its neighbours' speaker.
const NEAREST_MS: u64 = 2000;

fn overlap(a: (u64, u64), b: (u64, u64)) -> u64 {
    a.1.min(b.1).saturating_sub(a.0.max(b.0))
}

/// The diarizer speaker covering most of `span`, else the nearest turn
/// within [`NEAREST_MS`].
fn speaker_at(segments: &[Segment], span: (u64, u64)) -> Option<usize> {
    let mut by: HashMap<usize, u64> = HashMap::new();
    for s in segments {
        let o = overlap(span, (s.start_ms, s.end_ms));
        if o > 0 {
            *by.entry(s.speaker).or_default() += o;
        }
    }
    if let Some((&k, _)) = by.iter().max_by_key(|(&k, &o)| (o, std::cmp::Reverse(k))) {
        return Some(k);
    }
    segments
        .iter()
        .map(|s| {
            let gap = if s.end_ms <= span.0 {
                span.0 - s.end_ms
            } else {
                s.start_ms.saturating_sub(span.1)
            };
            (gap, s.speaker)
        })
        .filter(|&(gap, _)| gap <= NEAREST_MS)
        .min()
        .map(|(_, k)| k)
}

/// Per-word diarizer speakers for one paragraph: gaps filled from the
/// neighbours, and a lone word between two turns of the same speaker
/// (a straddling or mistimed word) folded into them.
fn word_speakers(words: &[Word], segments: &[Segment]) -> Vec<Option<usize>> {
    let mut out: Vec<Option<usize>> = words
        .iter()
        .map(|w| speaker_at(segments, (w.start_ms, w.end_ms.max(w.start_ms + 1))))
        .collect();
    for i in 1..out.len() {
        if out[i].is_none() {
            out[i] = out[i - 1];
        }
    }
    for i in (0..out.len().saturating_sub(1)).rev() {
        if out[i].is_none() {
            out[i] = out[i + 1];
        }
    }
    for i in 1..out.len().saturating_sub(1) {
        if out[i - 1] == out[i + 1] && out[i] != out[i - 1] {
            out[i] = out[i - 1];
        }
    }
    out
}

/// `p` with every clip and the paragraph itself labelled `speaker`.
fn labelled(p: &Paragraph, speaker: Option<String>) -> Paragraph {
    let mut q = p.clone();
    for c in &mut q.clips {
        c.speaker = speaker.clone();
    }
    q.speaker = speaker;
    q
}

/// A hardened paragraph holding one speaker's run of `p`'s words.
fn split_part(p: &Paragraph, n: usize, words: Vec<Word>, speaker: Option<String>) -> Paragraph {
    let id = if n == 0 {
        p.id.clone()
    } else {
        format!("{}-{n}", p.id)
    };
    let text = words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let start_ms = words.first().map(|w| w.start_ms).unwrap_or(p.start_ms);
    let end_ms = words
        .last()
        .map(|w| w.end_ms)
        .unwrap_or(p.end_ms)
        .max(start_ms);
    let clip = Clip {
        id: format!("{id}-clip"),
        start_ms,
        duration_ms: Some(end_ms - start_ms),
        text: text.clone(),
        stage: Stage::Revised,
        words: words.clone(),
        speaker: speaker.clone(),
    };
    Paragraph {
        id,
        start_ms,
        end_ms,
        text,
        clips: vec![clip],
        words,
        speaker,
        closed: true,
        hardened: true,
        pass3_inflight: false,
        pass4: None,
    }
}

/// `t` relabelled from `segments`. Paragraphs split where the speaker
/// changes mid-paragraph; text and word timings are otherwise unchanged.
/// With no segments the transcript is returned as is.
pub fn relabel(t: &Transcript, segments: &[Segment]) -> Transcript {
    if segments.is_empty() {
        return t.clone();
    }
    // Per paragraph: diarizer speaker runs over its words, or one speaker
    // for the whole paragraph when it has no word timings.
    let runs: Vec<Vec<(Option<usize>, Vec<Word>)>> = t
        .paragraphs
        .iter()
        .map(|p| {
            if p.words.is_empty() {
                let k = speaker_at(segments, (p.start_ms, p.end_ms.max(p.start_ms + 1)));
                return vec![(k, Vec::new())];
            }
            let mut runs: Vec<(Option<usize>, Vec<Word>)> = Vec::new();
            for (w, k) in p.words.iter().zip(word_speakers(&p.words, segments)) {
                match runs.last_mut() {
                    Some((rk, ws)) if *rk == k => ws.push(w.clone()),
                    _ => runs.push((k, vec![w.clone()])),
                }
            }
            runs
        })
        .collect();
    let mut names: HashMap<usize, String> = HashMap::new();
    for k in runs.iter().flatten().filter_map(|(k, _)| *k) {
        let next = label(names.len());
        names.entry(k).or_insert(next);
    }
    let name = |k: &Option<usize>| k.and_then(|k| names.get(&k).cloned());
    let mut out = Transcript::default();
    for (p, runs) in t.paragraphs.iter().zip(runs) {
        if runs.len() == 1 {
            let speaker = name(&runs[0].0).or(p.speaker.clone());
            out.paragraphs.push(labelled(p, speaker));
            continue;
        }
        for (n, (k, words)) in runs.into_iter().enumerate() {
            out.paragraphs.push(split_part(p, n, words, name(&k)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(t: &str, s: u64, e: u64) -> Word {
        Word {
            text: t.into(),
            start_ms: s,
            end_ms: e,
        }
    }

    fn para(id: &str, words: Vec<Word>, live: Option<&str>) -> Paragraph {
        let text = words
            .iter()
            .map(|w| w.text.clone())
            .collect::<Vec<_>>()
            .join(" ");
        let (s, e) = (words[0].start_ms, words.last().unwrap().end_ms);
        let clip = Clip {
            id: format!("{id}c"),
            start_ms: s,
            duration_ms: Some(e - s),
            text: text.clone(),
            stage: Stage::Final,
            words: words.clone(),
            speaker: live.map(String::from),
        };
        Paragraph {
            id: id.into(),
            start_ms: s,
            end_ms: e,
            text,
            clips: vec![clip],
            words,
            speaker: live.map(String::from),
            closed: true,
            hardened: true,
            pass3_inflight: false,
            pass4: None,
        }
    }

    fn seg(s: u64, e: u64, k: usize) -> Segment {
        Segment {
            start_ms: s,
            end_ms: e,
            speaker: k,
        }
    }

    #[test]
    fn splits_at_speaker_change_in_order_of_appearance() {
        // Live tagging heard one speaker, "B"; the diarizer finds two.
        let t = Transcript {
            paragraphs: vec![para(
                "p",
                vec![
                    word("Hello", 0, 400),
                    word("there.", 400, 900),
                    word("Hi", 1500, 1800),
                    word("back.", 1800, 2200),
                ],
                Some("B"),
            )],
        };
        let r = relabel(&t, &[seg(0, 1000, 7), seg(1400, 2300, 3)]);
        assert_eq!(r.paragraphs.len(), 2);
        assert_eq!(r.paragraphs[0].speaker.as_deref(), Some("A"));
        assert_eq!(r.paragraphs[0].text, "Hello there.");
        assert_eq!(r.paragraphs[0].id, "p");
        assert_eq!(r.paragraphs[1].speaker.as_deref(), Some("B"));
        assert_eq!(r.paragraphs[1].text, "Hi back.");
        assert_eq!(r.paragraphs[1].start_ms, 1500);
        assert!(r.paragraphs.iter().all(|p| p.hardened));
    }

    #[test]
    fn lone_straddling_word_follows_neighbours() {
        let t = Transcript {
            paragraphs: vec![para(
                "p",
                vec![
                    word("one", 0, 300),
                    word("two", 300, 600),
                    word("three", 600, 900),
                ],
                None,
            )],
        };
        let r = relabel(&t, &[seg(0, 450, 0), seg(450, 650, 1), seg(650, 900, 0)]);
        assert_eq!(r.paragraphs.len(), 1);
        assert_eq!(r.paragraphs[0].speaker.as_deref(), Some("A"));
        assert_eq!(r.paragraphs[0].clips[0].speaker.as_deref(), Some("A"));
    }

    #[test]
    fn words_outside_turns_take_nearest() {
        let t = Transcript {
            paragraphs: vec![
                para("p", vec![word("early", 0, 300)], None),
                para("q", vec![word("late", 9000, 9300)], None),
            ],
        };
        let r = relabel(&t, &[seg(500, 800, 0), seg(9500, 9900, 1)]);
        assert_eq!(r.paragraphs[0].speaker.as_deref(), Some("A"));
        assert_eq!(r.paragraphs[1].speaker.as_deref(), Some("B"));
    }

    #[test]
    fn no_segments_is_identity() {
        let t = Transcript {
            paragraphs: vec![para("p", vec![word("x", 0, 100)], Some("C"))],
        };
        let r = relabel(&t, &[]);
        assert_eq!(r.paragraphs[0].speaker.as_deref(), Some("C"));
    }
}
