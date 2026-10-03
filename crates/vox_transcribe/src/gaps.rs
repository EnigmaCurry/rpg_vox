//! Recovering speech an offline decode skipped. Parakeet's TDT decoder
//! sometimes stops emitting partway through a buffer (seen after a
//! change of voice) and silently drops the rest; decoding that stretch on
//! its own brings it back. So any span of clear speech that the decode's
//! word timings leave uncovered is decoded again and merged in.

use crate::filters::is_junk;
use crate::recognizer::{OfflineRecognizer, Transcription};
use crate::timing::{offset, Word};

/// Uncovered spans shorter than this are ordinary pauses.
const MIN_GAP_MS: u64 = 1000;
/// Share of 20 ms windows above the VAD threshold for a span to count as
/// speech.
const VOICED_SHARE: f32 = 0.4;
/// Audio kept on each side of a re-decoded span.
const PAD_MS: u64 = 60;

/// Spans (ms from the buffer start) of at least [`MIN_GAP_MS`] that no
/// word covers but that are mostly voiced.
pub fn uncovered_speech(
    words: &[Word],
    samples: &[f32],
    rate: u32,
    rms_threshold: f32,
) -> Vec<(u64, u64)> {
    let total = samples.len() as u64 * 1000 / rate.max(1) as u64;
    let mut edges = vec![0u64];
    for w in words {
        edges.push(w.start_ms);
        edges.push(w.end_ms);
    }
    edges.push(total);
    edges
        .chunks(2)
        .filter_map(|e| match e {
            [s, t] if t.saturating_sub(*s) >= MIN_GAP_MS => Some((*s, *t)),
            _ => None,
        })
        .filter(|&(s, e)| voiced_share(samples, rate, s, e, rms_threshold) >= VOICED_SHARE)
        .collect()
}

fn voiced_share(samples: &[f32], rate: u32, s: u64, e: u64, threshold: f32) -> f32 {
    let at = |ms: u64| ((ms * rate as u64 / 1000) as usize).min(samples.len());
    let span = &samples[at(s)..at(e)];
    let win = (rate as usize / 50).max(1);
    let (mut voiced, mut n) = (0usize, 0usize);
    for w in span.chunks(win).filter(|w| w.len() == win) {
        let rms = (w.iter().map(|x| x * x).sum::<f32>() / w.len() as f32).sqrt();
        voiced += (rms > threshold) as usize;
        n += 1;
    }
    if n == 0 {
        0.0
    } else {
        voiced as f32 / n as f32
    }
}

/// `rec`'s timed decode of `samples`, with any speech it skipped decoded
/// separately and merged in. Decodes without word timings are returned
/// as they are.
pub fn transcribe_covering(
    rec: &dyn OfflineRecognizer,
    samples: &[f32],
    rate: u32,
    rms_threshold: f32,
) -> anyhow::Result<Transcription> {
    let mut t = rec.transcribe_timed(samples, rate)?;
    if t.words.is_empty() {
        return Ok(t);
    }
    let total = samples.len() as u64 * 1000 / rate.max(1) as u64;
    let at = |ms: u64| ((ms * rate as u64 / 1000) as usize).min(samples.len());
    let mut found = false;
    for (s, e) in uncovered_speech(&t.words, samples, rate, rms_threshold) {
        let (from, to) = (s.saturating_sub(PAD_MS), (e + PAD_MS).min(total));
        let Ok(mut r) = rec.transcribe_timed(&samples[at(from)..at(to)], rate) else {
            continue;
        };
        if r.words.is_empty() || is_junk(r.text.trim()) {
            continue;
        }
        offset(&mut r.words, from);
        // Keep only what falls in the gap, not the padding's neighbours.
        r.words
            .retain(|w| (w.start_ms + w.end_ms) / 2 >= s && (w.start_ms + w.end_ms) / 2 <= e);
        if !r.words.is_empty() {
            tracing::debug!(from_ms = s, to_ms = e, recovered = %r.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" "), "decode skipped speech; recovered");
            t.words.extend(r.words);
            found = true;
        }
    }
    if found {
        t.words.sort_by_key(|w| w.start_ms);
        t.text = t
            .words
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 16_000;

    fn word(t: &str, s: u64, e: u64) -> Word {
        Word {
            text: t.into(),
            start_ms: s,
            end_ms: e,
        }
    }

    /// `ms` of tone (voiced) or silence.
    fn audio(spec: &[(u64, bool)]) -> Vec<f32> {
        spec.iter()
            .flat_map(|&(ms, on)| {
                (0..(ms * SR as u64 / 1000) as usize).map(move |i| {
                    if on {
                        0.2 * (i as f32 * 0.3).sin()
                    } else {
                        0.0
                    }
                })
            })
            .collect()
    }

    #[test]
    fn finds_voiced_tail_only() {
        // 0-2 s speech (decoded), 2-3 s silence, 3-6 s speech (skipped).
        let a = audio(&[(2000, true), (1000, false), (3000, true)]);
        let words = [word("a", 0, 900), word("b", 900, 1900)];
        assert_eq!(uncovered_speech(&words, &a, SR, 0.008), vec![(1900, 6000)]);
        // Fully decoded: nothing.
        let words = [word("a", 0, 1900), word("b", 3000, 6000)];
        assert!(uncovered_speech(&words, &a, SR, 0.008).is_empty());
    }

    /// Decodes the full buffer as one word ending at 2 s; any other
    /// buffer as "x y" over its whole length.
    struct Skipper;

    impl OfflineRecognizer for Skipper {
        fn transcribe(&self, _: &[f32], _: u32) -> anyhow::Result<String> {
            unreachable!()
        }
        fn transcribe_timed(&self, s: &[f32], rate: u32) -> anyhow::Result<Transcription> {
            let ms = s.len() as u64 * 1000 / rate as u64;
            Ok(if ms >= 6000 {
                Transcription {
                    text: "Hello.".into(),
                    words: vec![word("Hello.", 0, 2000)],
                }
            } else {
                Transcription {
                    text: "x y".into(),
                    words: vec![word("x", 0, ms / 2), word("y", ms / 2, ms)],
                }
            })
        }
    }

    #[test]
    fn merges_recovered_words() {
        let a = audio(&[(6000, true)]);
        let t = transcribe_covering(&Skipper, &a, SR, 0.008).unwrap();
        assert_eq!(t.text, "Hello. x y");
        assert_eq!(t.words.len(), 3);
        assert!(t.words[1].start_ms >= 2000 - PAD_MS);
    }
}
