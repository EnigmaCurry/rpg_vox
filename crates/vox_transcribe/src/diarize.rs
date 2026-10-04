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

/// Per paragraph, the diarizer speaker of each word (one entry for a
/// paragraph without word timings).
type Labels = Vec<Vec<Option<usize>>>;

fn word_labels(t: &Transcript, segments: &[Segment]) -> Labels {
    t.paragraphs
        .iter()
        .map(|p| {
            if p.words.is_empty() {
                vec![speaker_at(
                    segments,
                    (p.start_ms, p.end_ms.max(p.start_ms + 1)),
                )]
            } else {
                word_speakers(&p.words, segments)
            }
        })
        .collect()
}

/// `t` relabelled from `segments`. Paragraphs split where the speaker
/// changes mid-paragraph; text and word timings are otherwise unchanged.
/// With no segments the transcript is returned as is.
pub fn relabel(t: &Transcript, segments: &[Segment]) -> Transcript {
    if segments.is_empty() {
        return t.clone();
    }
    build(t, &word_labels(t, segments))
}

/// Embeds the audio between two times (ms of audio), for
/// [`relabel_refined`]. `None` when it can't (too short, out of range).
pub type Embed<'a> = dyn FnMut(u64, u64) -> Option<Vec<f32>> + 'a;

#[derive(Debug, Clone)]
pub struct RefineConfig {
    /// Units shorter than this aren't embedded; they take the majority
    /// diarizer speaker of their words instead.
    pub min_unit_ms: u64,
    /// A diarizer speaker change mid-sentence moves to the best pause or
    /// clause break within this many words.
    pub snap_words: usize,
    /// Added to the cosine similarity of the diarizer's own choice for a
    /// unit, so a near tie goes its way.
    pub diarizer_bonus: f32,
    /// Segments embedded per speaker to build its first voice profile.
    pub profile_segments: usize,
    /// Assign-and-reprofile rounds.
    pub rounds: usize,
    /// Keep this many speakers (`--speakers N`): the diarizer's N biggest
    /// clusters. Without it, clusters under `min_speaker_ms` are dropped.
    pub max_speakers: Option<usize>,
    /// Total speech below which a cluster is a stray (laughter, a jingle,
    /// a cough) rather than a speaker: `min_speaker_ms`, or less in a
    /// short recording (`min_speaker_share` of all its speech).
    pub min_speaker_ms: u64,
    pub min_speaker_share: f32,
}

impl Default for RefineConfig {
    fn default() -> Self {
        Self {
            min_unit_ms: 700,
            snap_words: 4,
            diarizer_bonus: 0.03,
            profile_segments: 12,
            rounds: 4,
            max_speakers: None,
            min_speaker_ms: 10_000,
            min_speaker_share: 0.05,
        }
    }
}

/// Like [`relabel`], then tightened at the sentence level. The diarizer's
/// turn boundaries are only accurate to a few hundred ms, so when people
/// speak in quick succession the first or last words of a turn can land
/// on the wrong side, and a whole short turn can merge into its
/// neighbour. So: each paragraph is cut into units at sentence ends and
/// at diarizer changes (moved to the nearest pause or clause break); each
/// unit long enough to judge is matched by its own voice embedding
/// against a profile of every diarizer speaker, and shorter units take
/// the majority speaker of their words.
pub fn relabel_refined(
    t: &Transcript,
    segments: &[Segment],
    embed: &mut Embed,
    cfg: &RefineConfig,
) -> Transcript {
    if segments.is_empty() {
        return t.clone();
    }
    let mut profiles = profiles(segments, embed, cfg);
    let segments = &consolidate(segments, &mut profiles, cfg);
    let mut labels = word_labels(t, segments);
    // (paragraph, words a..b, unit embedding if long enough)
    let mut all: Vec<(usize, usize, usize, Option<Vec<f32>>)> = Vec::new();
    for (pi, (p, ks)) in t.paragraphs.iter().zip(&labels).enumerate() {
        if p.words.is_empty() {
            continue;
        }
        for (a, b) in units(&p.words, ks, cfg.snap_words) {
            let (start, end) = (p.words[a].start_ms, p.words[b - 1].end_ms);
            let emb = (end.saturating_sub(start) >= cfg.min_unit_ms)
                .then(|| embed(start, end))
                .flatten()
                .and_then(|e| unit_vec(&e));
            all.push((pi, a, b, emb));
        }
    }
    // Give each unit the speaker whose profile its voice is closest to
    // (plus a little for the diarizer's own choice; a unit too short to
    // embed keeps that choice). Then rebuild the profiles from the units
    // each speaker got (one voice each, unlike a diarizer segment that
    // ran two turns together) and assign again, until nothing changes.
    let diarized: Vec<Option<usize>> = all
        .iter()
        .map(|(pi, a, b, _)| majority(&labels[*pi][*a..*b]))
        .collect();
    let mut assigned: Vec<Option<usize>> = diarized.clone();
    if profiles.len() > 1 {
        for _ in 0..cfg.rounds {
            let next: Vec<Option<usize>> = all
                .iter()
                .zip(&diarized)
                .map(|((_, _, _, emb), &d)| {
                    let Some(e) = emb else { return d };
                    profiles
                        .iter()
                        .map(|(k, p)| {
                            let bonus = if d == Some(*k) {
                                cfg.diarizer_bonus
                            } else {
                                0.0
                            };
                            (dot(p, e) + bonus, *k)
                        })
                        .max_by(|a, b| a.0.total_cmp(&b.0))
                        .map(|(_, k)| k)
                })
                .collect();
            if next == assigned {
                break;
            }
            assigned = next;
            let mut sums: HashMap<usize, Vec<f32>> = HashMap::new();
            for ((_, _, _, emb), k) in all.iter().zip(&assigned) {
                if let (Some(e), Some(k)) = (emb, k) {
                    let acc = sums.entry(*k).or_insert_with(|| vec![0.0; e.len()]);
                    acc.iter_mut().zip(e).for_each(|(s, x)| *s += x);
                }
            }
            for (k, p) in profiles.iter_mut() {
                if let Some(v) = sums.get(k).and_then(|v| unit_vec(v)) {
                    *p = v;
                }
            }
        }
    }
    for ((pi, a, b, _), k) in all.iter().zip(&assigned) {
        if k.is_some() {
            labels[*pi][*a..*b].iter_mut().for_each(|x| *x = *k);
        }
    }
    for ((pi, a, b, emb), (d, k)) in all.iter().zip(diarized.iter().zip(&assigned)) {
        let words = &t.paragraphs[*pi].words[*a..*b];
        let sims: Vec<(usize, f32)> = emb
            .iter()
            .flat_map(|e| profiles.iter().map(move |(k, p)| (*k, dot(p, e))))
            .collect();
        tracing::debug!(
            start_ms = words[0].start_ms,
            end_ms = words[words.len() - 1].end_ms,
            diarizer = ?d,
            speaker = ?k,
            ?sims,
            text = %words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" "),
            "speaker unit"
        );
    }
    build(t, &labels)
}

/// Fold stray clusters into real speakers. The diarizer is run without
/// a fixed count (forcing one makes it merge two real voices and keep
/// a stray as the other "speaker"), so it can return extra clusters of a
/// few seconds each. Keep the `max_speakers` biggest, or every cluster
/// with enough speech (see [`RefineConfig::min_speaker_ms`]), and
/// give each other cluster's segments to the kept speaker whose voice
/// profile is closest. `profiles` is cut down to the kept speakers.
fn consolidate(
    segments: &[Segment],
    profiles: &mut Vec<(usize, Vec<f32>)>,
    cfg: &RefineConfig,
) -> Vec<Segment> {
    let mut total: HashMap<usize, u64> = HashMap::new();
    for s in segments {
        *total.entry(s.speaker).or_default() += s.end_ms.saturating_sub(s.start_ms);
    }
    let all: u64 = total.values().sum();
    let min_ms = cfg
        .min_speaker_ms
        .min((all as f64 * cfg.min_speaker_share as f64) as u64);
    let mut by_size: Vec<(usize, u64)> = total.into_iter().collect();
    by_size.sort_by_key(|&(k, ms)| (std::cmp::Reverse(ms), k));
    let keep: Vec<usize> = match cfg.max_speakers {
        Some(n) => by_size.iter().take(n.max(1)).map(|&(k, _)| k).collect(),
        None => by_size
            .iter()
            .enumerate()
            .filter(|&(i, &(_, ms))| i == 0 || ms >= min_ms)
            .map(|(_, &(k, _))| k)
            .collect(),
    };
    let profile = |k: usize| profiles.iter().find(|(pk, _)| *pk == k).map(|(_, p)| p);
    let mut map: HashMap<usize, usize> = HashMap::new();
    for &(k, _) in &by_size {
        let to = if keep.contains(&k) {
            k
        } else {
            profile(k)
                .and_then(|p| {
                    keep.iter()
                        .filter_map(|&j| profile(j).map(|q| (dot(p, q), j)))
                        .max_by(|a, b| a.0.total_cmp(&b.0))
                        .map(|(_, j)| j)
                })
                .unwrap_or(keep[0])
        };
        if to != k {
            tracing::debug!(from = k, to, "stray speaker cluster folded in");
        }
        map.insert(k, to);
    }
    profiles.retain(|(k, _)| keep.contains(k));
    segments
        .iter()
        .map(|s| Segment {
            speaker: map[&s.speaker],
            ..*s
        })
        .collect()
}

/// A voice profile per diarizer speaker: the normalized mean embedding
/// of its longest segments.
fn profiles(segments: &[Segment], embed: &mut Embed, cfg: &RefineConfig) -> Vec<(usize, Vec<f32>)> {
    let mut by: HashMap<usize, Vec<&Segment>> = HashMap::new();
    for s in segments {
        by.entry(s.speaker).or_default().push(s);
    }
    let mut out = Vec::new();
    for (k, mut segs) in by {
        segs.sort_by_key(|s| std::cmp::Reverse(s.end_ms.saturating_sub(s.start_ms)));
        let mut sum: Option<Vec<f32>> = None;
        for s in segs.into_iter().take(cfg.profile_segments) {
            let Some(e) = embed(s.start_ms, s.end_ms).and_then(|e| unit_vec(&e)) else {
                continue;
            };
            match &mut sum {
                Some(acc) => acc.iter_mut().zip(&e).for_each(|(a, x)| *a += x),
                None => sum = Some(e),
            }
        }
        if let Some(p) = sum.and_then(|v| unit_vec(&v)) {
            out.push((k, p));
        }
    }
    out.sort_by_key(|(k, _)| *k);
    out
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn unit_vec(v: &[f32]) -> Option<Vec<f32>> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    (n > 1e-6 && n.is_finite()).then(|| v.iter().map(|x| x / n).collect())
}

fn majority(ks: &[Option<usize>]) -> Option<usize> {
    let mut count: HashMap<usize, usize> = HashMap::new();
    for k in ks.iter().flatten() {
        *count.entry(*k).or_default() += 1;
    }
    count
        .into_iter()
        .max_by_key(|&(k, n)| (n, std::cmp::Reverse(k)))
        .map(|(k, _)| k)
}

fn ends_sentence(w: &str) -> bool {
    w.trim_end_matches(['"', '\'', ')', '”', '’'])
        .ends_with(['.', '?', '!'])
}

fn ends_clause(w: &str) -> bool {
    w.ends_with([',', ';', ':', '—'])
}

/// Word ranges `[a, b)` of a paragraph to label as one: cut at sentence
/// ends, and at each diarizer change that has no sentence end within
/// `snap` words, moved to the best nearby pause or clause break.
fn units(words: &[Word], ks: &[Option<usize>], snap: usize) -> Vec<(usize, usize)> {
    let n = words.len();
    // A cut at `i` falls between words i-1 and i.
    let mut cuts: Vec<usize> = (1..n)
        .filter(|&i| ends_sentence(&words[i - 1].text))
        .collect();
    for c in (1..n).filter(|&i| ks[i] != ks[i - 1]) {
        let lo = c.saturating_sub(snap).max(1);
        let hi = (c + snap).min(n - 1);
        if cuts.iter().any(|&x| x >= lo && x <= hi) {
            continue;
        }
        let score = |i: usize| {
            let gap = words[i].start_ms.saturating_sub(words[i - 1].end_ms) as i64;
            let clause = if ends_clause(&words[i - 1].text) {
                150
            } else {
                0
            };
            gap + clause - 30 * (i as i64 - c as i64).abs()
        };
        if let Some(best) = (lo..=hi).max_by_key(|&i| (score(i), std::cmp::Reverse(i))) {
            cuts.push(best);
        }
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut out = Vec::with_capacity(cuts.len() + 1);
    let mut a = 0;
    for c in cuts.into_iter().chain([n]) {
        if c > a {
            out.push((a, c));
            a = c;
        }
    }
    out
}

/// Words in a row that make a speaker's first appearance count for
/// lettering (see [`build`]).
const SUBSTANTIAL_WORDS: usize = 6;

/// The output transcript from per-word diarizer labels: runs of one
/// speaker become paragraphs, lettered in order of first appearance.
fn build(t: &Transcript, labels: &Labels) -> Transcript {
    let runs: Vec<Vec<(Option<usize>, Vec<Word>)>> = t
        .paragraphs
        .iter()
        .zip(labels)
        .map(|(p, ks)| {
            if p.words.is_empty() {
                return vec![(ks.first().copied().flatten(), Vec::new())];
            }
            let mut runs: Vec<(Option<usize>, Vec<Word>)> = Vec::new();
            for (w, &k) in p.words.iter().zip(ks) {
                match runs.last_mut() {
                    Some((rk, ws)) if *rk == k => ws.push(w.clone()),
                    _ => runs.push((k, vec![w.clone()])),
                }
            }
            runs
        })
        .collect();
    // Letter speakers by when they first say something substantial, so a
    // jingle or a stray word folded into one speaker at the very start
    // doesn't make them "A" (and take the first --speakers name).
    let mut names: HashMap<usize, String> = HashMap::new();
    let substantial = runs
        .iter()
        .flatten()
        .filter(|(_, ws)| ws.len() >= SUBSTANTIAL_WORDS)
        .filter_map(|(k, _)| *k);
    for k in substantial.chain(runs.iter().flatten().filter_map(|(k, _)| *k)) {
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

    #[test]
    fn units_cut_at_sentences_and_snap_changes_to_pauses() {
        let ws = vec![
            word("one", 0, 200),
            word("two.", 200, 400),
            word("three", 400, 600),
            word("four,", 600, 800),
            word("five", 1300, 1500),
            word("six", 1500, 1700),
        ];
        // The diarizer flips one word late (at "six"); the pause before
        // "five" wins.
        let ks = [Some(0), Some(0), Some(0), Some(0), Some(0), Some(1)];
        assert_eq!(units(&ws, &ks, 4), vec![(0, 2), (2, 6)]);
        let ks = [Some(0), Some(0), Some(0), Some(0), Some(0), Some(0)];
        assert_eq!(units(&ws, &ks, 4), vec![(0, 2), (2, 6)]);
        // No sentence end near the flip: it moves to the pause.
        let ks = [Some(0), Some(0), Some(0), Some(0), Some(1), Some(1)];
        let ws2: Vec<Word> = ws
            .iter()
            .map(|w| word(w.text.trim_end_matches('.'), w.start_ms, w.end_ms))
            .collect();
        assert_eq!(units(&ws2, &ks, 4), vec![(0, 4), (4, 6)]);
    }

    #[test]
    fn refined_fixes_boundary_words_and_merged_turns() {
        // Two sentences; the diarizer puts the second speaker's first two
        // words with the first speaker, and then the whole of a third
        // sentence that the embeddings say is speaker 1.
        let t = Transcript {
            paragraphs: vec![para(
                "p",
                vec![
                    word("Hello", 0, 500),
                    word("there.", 500, 1500),
                    word("I'll", 1600, 1900),
                    word("keep", 1900, 2200),
                    word("watch.", 2200, 3000),
                    word("Over", 3100, 3600),
                    word("here.", 3600, 4500),
                ],
                None,
            )],
        };
        let segs = [
            seg(0, 2200, 0),
            seg(2200, 4500, 1),
            seg(5000, 9000, 0),
            seg(9000, 12000, 1),
        ];
        // Voices by time: speaker 0 before 1550 ms, speaker 1 after.
        let mut embed = |s: u64, e: u64| {
            let mid = (s + e) / 2;
            Some(if (mid < 1550) || (5000..9000).contains(&mid) {
                vec![1.0, 0.0]
            } else {
                vec![0.0, 1.0]
            })
        };
        let cfg = RefineConfig {
            min_speaker_ms: 0,
            ..Default::default()
        };
        let r = relabel_refined(&t, &segs, &mut embed, &cfg);
        let texts: Vec<(&str, Option<&str>)> = r
            .paragraphs
            .iter()
            .map(|p| (p.text.as_str(), p.speaker.as_deref()))
            .collect();
        assert_eq!(
            texts,
            vec![
                ("Hello there.", Some("A")),
                ("I'll keep watch. Over here.", Some("B")),
            ]
        );
        // Plain relabel splits mid-sentence.
        assert_eq!(relabel(&t, &segs).paragraphs.len(), 2);
        assert_eq!(
            relabel(&t, &segs).paragraphs[0].text,
            "Hello there. I'll keep"
        );
    }

    #[test]
    fn strays_fold_into_the_closest_kept_speaker() {
        let segs = [
            seg(0, 30_000, 0),
            seg(30_000, 31_000, 5),
            seg(31_000, 60_000, 1),
            seg(60_000, 62_000, 7),
        ];
        let mut profiles = vec![
            (0, vec![1.0, 0.0]),
            (1, vec![0.0, 1.0]),
            (5, vec![0.1, 0.9]),
            (7, vec![0.9, 0.1]),
        ];
        let out = consolidate(&segs, &mut profiles, &RefineConfig::default());
        let ks: Vec<usize> = out.iter().map(|s| s.speaker).collect();
        assert_eq!(ks, vec![0, 1, 1, 0]);
        assert_eq!(profiles.len(), 2);
        // --speakers 1: everything goes to the biggest.
        let mut profiles = vec![(0, vec![1.0, 0.0]), (1, vec![0.0, 1.0])];
        let cfg = RefineConfig {
            max_speakers: Some(1),
            ..Default::default()
        };
        let out = consolidate(&segs[..3], &mut profiles, &cfg);
        assert!(out.iter().all(|s| s.speaker == 0 || s.speaker == 1));
        assert_eq!(profiles.len(), 1);
    }
}
