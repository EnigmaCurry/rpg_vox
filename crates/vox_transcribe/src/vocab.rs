//! Custom vocabulary (`--vocab`): proper nouns the recognizer can't know.
//!
//! A decoder biased toward the terms (sherpa-onnx hotwords) catches them,
//! but biasing is prone to false positives: pushed hard, it hears the
//! names in ordinary speech. So a term from the biased decode is only
//! kept where an unbiased decode of the same audio heard something that
//! *sounds like* it; elsewhere the unbiased words stand. A name can then
//! fix a mishearing of itself, but never appear out of nothing.

use std::sync::Arc;

use crate::recognizer::{OfflineRecognizer, Transcription};
use crate::timing::{align_words, Word};

/// Hotword boost per matched token. Tuned on synthetic Curse of Strahd
/// dialogue: 2.5 caught 13 of 20 names (3 unbiased). Sound-alike
/// sentences gained no names except "Tatiana" → "Tatyana", a homophone
/// no acoustic check can tell apart; 4 also turned "Is Mark" into
/// "Ismark".
pub const HOTWORD_SCORE: f32 = 2.5;
/// Active paths in the biased beam search.
pub const BEAM: i32 = 4;
/// How alike (see [`sounds_like`]) the unbiased words must be to a term.
pub const MIN_SIMILARITY: f32 = 0.6;

/// Normalized form for matching: lowercase letters and digits.
fn norm(w: &str) -> String {
    w.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[derive(Debug, Clone, Default)]
pub struct Vocab {
    /// Terms as written, e.g. "Strahd von Zarovich".
    pub terms: Vec<String>,
    /// Each term's normalized words.
    keys: Vec<Vec<String>>,
}

impl Vocab {
    /// One term per line; blank lines and `#` comments are skipped.
    pub fn parse(text: &str) -> Self {
        let terms: Vec<String> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect();
        Self::new(terms)
    }

    pub fn new(terms: Vec<String>) -> Self {
        let keys = terms
            .iter()
            .map(|t| {
                t.split_whitespace()
                    .map(norm)
                    .filter(|w| !w.is_empty())
                    .collect()
            })
            .collect();
        Self { terms, keys }
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Occurrences of terms in `words`: (first word, past last word, term).
    /// Longer terms win where they overlap.
    pub fn find(&self, words: &[Word]) -> Vec<(usize, usize, usize)> {
        let ws: Vec<String> = words.iter().map(|w| norm(&w.text)).collect();
        let mut order: Vec<usize> = (0..self.keys.len()).collect();
        order.sort_by_key(|&k| std::cmp::Reverse(self.keys[k].len()));
        let mut taken = vec![false; ws.len()];
        let mut out = Vec::new();
        for k in order {
            let key = &self.keys[k];
            if key.is_empty() || key.len() > ws.len() {
                continue;
            }
            for i in 0..=ws.len() - key.len() {
                let j = i + key.len();
                if ws[i..j] == key[..] && !taken[i..j].iter().any(|&t| t) {
                    taken[i..j].iter_mut().for_each(|t| *t = true);
                    out.push((i, j, k));
                }
            }
        }
        out.sort_unstable();
        out
    }
}

/// A rough phonetic key: consonant skeleton with common spellings of the
/// same sound merged and vowels collapsed, so "Strahd" and "strawed" or
/// "Barovia" and "bar over" come out alike.
pub fn phonetic(text: &str) -> String {
    let s: Vec<char> = norm(text).chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let push = |out: &mut String, c: char| {
        if !out.ends_with(c) {
            out.push(c);
        }
    };
    while i < s.len() {
        let c = s[i];
        let next = s.get(i + 1).copied();
        let vowel = |c: char| "aeiouy".contains(c);
        match (c, next) {
            ('p', Some('h')) => {
                push(&mut out, 'f');
                i += 1;
            }
            ('t' | 'd', Some('h')) => {
                push(&mut out, 't');
                i += 1;
            }
            ('s' | 'c', Some('h')) => {
                push(&mut out, 'x');
                i += 1;
            }
            ('c', Some('k')) => {
                push(&mut out, 'k');
                i += 1;
            }
            ('c', Some('e' | 'i' | 'y')) => push(&mut out, 's'),
            ('c' | 'q' | 'k' | 'g', _) => push(&mut out, 'k'),
            ('x', _) => {
                push(&mut out, 'k');
                out.push('s');
            }
            ('z', _) => push(&mut out, 's'),
            ('d', _) => push(&mut out, 't'),
            ('b', _) => push(&mut out, 'p'),
            ('v', _) => push(&mut out, 'f'),
            ('w' | 'h', _) if i > 0 => {}
            (c, _) if vowel(c) => {
                // Keep one marker for a vowel sound, none at the end.
                if i + 1 < s.len() {
                    push(&mut out, 'a');
                }
            }
            (c, _) => push(&mut out, c),
        }
        i += 1;
    }
    out
}

fn levenshtein(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + (ca != cb) as usize)
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// 0..1: how alike `a` and `b` sound, by their phonetic keys.
pub fn sounds_like(a: &str, b: &str) -> f32 {
    let (ka, kb): (Vec<char>, Vec<char>) =
        (phonetic(a).chars().collect(), phonetic(b).chars().collect());
    let len = ka.len().max(kb.len());
    if len == 0 {
        return 0.0;
    }
    1.0 - levenshtein(&ka, &kb) as f32 / len as f32
}

/// The words of `plain` that cover `biased[a..b]`, matched by an LCS over
/// both decodes (the same audio, so the words around a term agree).
fn counterpart(biased: &[Word], plain: &[Word], a: usize, b: usize) -> (usize, usize) {
    let x: Vec<String> = biased.iter().map(|w| norm(&w.text)).collect();
    let y: Vec<String> = plain.iter().map(|w| norm(&w.text)).collect();
    let mut t = vec![vec![0u32; y.len() + 1]; x.len() + 1];
    for i in (0..x.len()).rev() {
        for j in (0..y.len()).rev() {
            t[i][j] = if !x[i].is_empty() && x[i] == y[j] {
                t[i + 1][j + 1] + 1
            } else {
                t[i + 1][j].max(t[i][j + 1])
            };
        }
    }
    // matched[i] = j for biased word i matched to plain word j.
    let mut matched = vec![None; x.len()];
    let (mut i, mut j) = (0, 0);
    while i < x.len() && j < y.len() {
        if !x[i].is_empty() && x[i] == y[j] {
            matched[i] = Some(j);
            i += 1;
            j += 1;
        } else if t[i + 1][j] >= t[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    let from = (0..a)
        .rev()
        .find_map(|i| matched[i])
        .map(|j| j + 1)
        .unwrap_or(0);
    let to = (b..x.len()).find_map(|i| matched[i]).unwrap_or(y.len());
    (from, to.max(from))
}

/// `plain` with each vocabulary term of `biased` swapped in where the
/// plain decode heard something that sounds like it (`min_similarity`).
pub fn merge(
    biased: &Transcription,
    plain: &Transcription,
    vocab: &Vocab,
    min_similarity: f32,
) -> Transcription {
    let mut out: Vec<Word> = Vec::new();
    let mut next_plain = 0;
    let mut changed = false;
    for (a, b, k) in vocab.find(&biased.words) {
        let (pa, pb) = counterpart(&biased.words, &plain.words, a, b);
        if pa < next_plain {
            continue;
        }
        let heard: Vec<&str> = plain.words[pa..pb]
            .iter()
            .map(|w| w.text.as_str())
            .collect();
        let heard = heard.join(" ");
        let sim = sounds_like(&vocab.terms[k], &heard);
        let keep = !heard.is_empty() && sim >= min_similarity;
        tracing::debug!(term = %vocab.terms[k], %heard, sim, keep, "vocabulary");
        if !keep {
            continue;
        }
        out.extend_from_slice(&plain.words[next_plain..pa]);
        // The term's words as the biased decode wrote them (its own
        // punctuation and casing), timed over what the plain decode heard.
        let span = (
            plain.words[pa].start_ms,
            plain.words[pb - 1].end_ms.max(plain.words[pa].start_ms),
        );
        let text: Vec<&str> = biased.words[a..b].iter().map(|w| w.text.as_str()).collect();
        out.extend(align_words(&biased.words[a..b], &text.join(" "), span));
        next_plain = pb;
        changed = true;
    }
    if !changed {
        return plain.clone();
    }
    out.extend_from_slice(&plain.words[next_plain..]);
    let text = out
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    Transcription { text, words: out }
}

/// A recognizer that uses a vocabulary-biased decode, checked against
/// an unbiased one (see the module docs). The unbiased decode only runs
/// when the biased one produced a term.
pub struct VocabRecognizer {
    pub biased: Arc<dyn OfflineRecognizer>,
    pub plain: Arc<dyn OfflineRecognizer>,
    pub vocab: Vocab,
    pub min_similarity: f32,
}

impl OfflineRecognizer for VocabRecognizer {
    fn transcribe(&self, samples: &[f32], sample_rate: u32) -> anyhow::Result<String> {
        Ok(self.transcribe_timed(samples, sample_rate)?.text)
    }

    fn transcribe_timed(&self, samples: &[f32], sample_rate: u32) -> anyhow::Result<Transcription> {
        let biased = self.biased.transcribe_timed(samples, sample_rate)?;
        if self.vocab.find(&biased.words).is_empty() {
            return Ok(biased);
        }
        let plain = self.plain.transcribe_timed(samples, sample_rate)?;
        if plain.words.is_empty() {
            return Ok(plain);
        }
        Ok(merge(&biased, &plain, &self.vocab, self.min_similarity))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tr(text: &str) -> Transcription {
        let words = text
            .split_whitespace()
            .enumerate()
            .map(|(i, w)| Word {
                text: w.into(),
                start_ms: i as u64 * 300,
                end_ms: i as u64 * 300 + 250,
            })
            .collect();
        Transcription {
            text: text.into(),
            words,
        }
    }

    #[test]
    fn phonetic_keys() {
        assert!(sounds_like("Strahd", "strawed") >= 0.7);
        assert!(sounds_like("Barovia", "bar over") >= 0.6);
        assert!(sounds_like("Ireena", "Irina") >= 0.7);
        assert!(sounds_like("Strahd", "the castle") < 0.4);
    }

    #[test]
    fn finds_terms_longest_first() {
        let v = Vocab::parse("Strahd\nStrahd von Zarovich\n# comment\n\n");
        assert_eq!(v.terms.len(), 2);
        let w = tr("I am Strahd von Zarovich, and Strahd.").words;
        assert_eq!(v.find(&w), vec![(2, 5, 1), (6, 7, 0)]);
    }

    #[test]
    fn keeps_terms_only_over_soundalikes() {
        let v = Vocab::parse("Strahd\nBarovia");
        // The plain decode misheard the name: the term goes in.
        let m = merge(
            &tr("Count Strahd rules here."),
            &tr("Count strawed rules here."),
            &v,
            0.6,
        );
        assert_eq!(m.text, "Count Strahd rules here.");
        // The biased decode put a name over unrelated words: plain wins.
        let m = merge(
            &tr("Welcome to Barovia tonight."),
            &tr("Welcome to the castle tonight."),
            &v,
            0.6,
        );
        assert_eq!(m.text, "Welcome to the castle tonight.");
        assert_eq!(m.words.len(), 5);
    }
}
