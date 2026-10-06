//! `--play NAME` without NAME.opus: read NAME.md aloud with Kokoro.
//!
//! A background thread speaks the transcript a sentence at a time, each
//! speaker in a voice of its own, and appends the audio and a cue per
//! sentence to a [`Speech`] that the player reads while it grows. Word
//! times within a sentence are spread by length; Kokoro doesn't report
//! them.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use vox_transcribe::tts::{voice_id, Kokoro, CAST};

use crate::play::{Cue, KWord};

/// Speak no further than this ahead of the playhead.
const LOOKAHEAD_MS: u64 = 120_000;
/// Silence after a sentence, after a paragraph, and before a new speaker.
const SENTENCE_GAP_MS: u64 = 150;
const PARAGRAPH_GAP_MS: u64 = 400;
const TURN_GAP_MS: u64 = 600;

/// One paragraph of a transcript.
#[derive(Debug, PartialEq)]
pub struct Para {
    pub speaker: Option<String>,
    pub text: String,
}

/// The paragraphs of a scribe markdown file: `**[hh:mm:ss]** text` or
/// `**[hh:mm:ss] Name:** text`. Plain paragraphs are read as they are;
/// the `#` title, `---` rules and the italic date lines are skipped.
pub fn paragraphs(md: &str) -> Vec<Para> {
    md.split("\n\n")
        .map(str::trim)
        .filter(|b| !b.is_empty() && !b.starts_with('#') && *b != "---")
        .filter(|b| !(b.starts_with('*') && !b.starts_with("**") && b.ends_with('*')))
        .filter_map(|b| {
            let (speaker, text) = match b.strip_prefix("**[").and_then(|r| r.split_once("** ")) {
                Some((head, text)) => {
                    let who = head.split_once(']').map(|(_, w)| w).unwrap_or("");
                    let who = who.trim().trim_end_matches(':').trim();
                    ((!who.is_empty()).then(|| who.to_string()), text)
                }
                None => (None, b),
            };
            let text = text
                .replace(['*', '`'], "")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            (!text.is_empty()).then_some(Para { speaker, text })
        })
        .collect()
}

/// `text` cut after each `.`, `?` or `!` (and any closing quote) that is
/// followed by a space, so "d.rymcg.tech" stays whole.
pub fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for w in text.split_whitespace() {
        cur.push(w);
        if w.trim_end_matches(['"', '\'', ')', '”', '’'])
            .ends_with(['.', '?', '!'])
        {
            out.push(cur.join(" "));
            cur.clear();
        }
    }
    if !cur.is_empty() {
        out.push(cur.join(" "));
    }
    out
}

/// Word timings across `[start, end)` ms, each word's share by length
/// (plus one for the gap after it).
pub fn spread(words: &[&str], start: u64, end: u64) -> Vec<KWord> {
    let weights: Vec<u64> = words.iter().map(|w| w.chars().count() as u64 + 1).collect();
    let total = weights.iter().sum::<u64>().max(1);
    let span = end.saturating_sub(start);
    let mut acc = 0;
    words
        .iter()
        .zip(&weights)
        .map(|(w, &n)| {
            let s = start + span * acc / total;
            acc += n;
            KWord {
                text: w.to_string(),
                start_ms: s,
                end_ms: start + span * acc / total,
            }
        })
        .collect()
}

#[derive(Default)]
struct State {
    samples: Vec<f32>,
    cues: Vec<Cue>,
    done: bool,
    error: Option<String>,
}

/// Speech produced so far, shared between the speaking thread and the
/// player.
pub struct Speech {
    pub rate: u32,
    /// Sentences in the whole transcript, for progress.
    pub total: usize,
    state: Mutex<State>,
    /// Where the player is, so the speaker stays [`LOOKAHEAD_MS`] ahead.
    playhead_ms: AtomicU64,
    stop: AtomicBool,
}

impl Speech {
    pub fn len(&self) -> usize {
        self.state.lock().expect("speech lock").samples.len()
    }

    /// Up to `max` samples from `pos` appended to `out`: `Some(n)` with
    /// the count (0 while the next sentence is still being spoken), or
    /// `None` once everything has been read.
    pub fn read(&self, pos: usize, max: usize, out: &mut Vec<f32>) -> Option<usize> {
        let s = self.state.lock().expect("speech lock");
        if pos >= s.samples.len() {
            return (!s.done).then_some(0);
        }
        let end = (pos + max).min(s.samples.len());
        out.extend_from_slice(&s.samples[pos..end]);
        Some(end - pos)
    }

    /// Cues after the first `have`.
    pub fn cues_from(&self, have: usize) -> Vec<Cue> {
        let s = self.state.lock().expect("speech lock");
        s.cues.get(have..).map(<[Cue]>::to_vec).unwrap_or_default()
    }

    pub fn done(&self) -> bool {
        self.state.lock().expect("speech lock").done
    }

    pub fn error(&self) -> Option<String> {
        self.state.lock().expect("speech lock").error.clone()
    }

    pub fn set_playhead(&self, ms: u64) {
        self.playhead_ms.store(ms, Ordering::Relaxed);
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Load Kokoro from `model_dir` and start reading `md` aloud. `voices`
/// override [`CAST`] for speakers in order of first appearance.
pub fn start(md: &str, model_dir: &Path, voices: &[String]) -> Result<Arc<Speech>> {
    let paras = paragraphs(md);
    if paras.is_empty() {
        bail!("nothing to read in the transcript");
    }
    let mut cast: Vec<i32> = Vec::new();
    for v in voices {
        cast.push(voice_id(v).with_context(|| {
            format!(
                "no Kokoro voice {v:?}; try one of {}",
                vox_transcribe::tts::VOICES[..28].join(", ")
            )
        })?);
    }
    for v in CAST {
        cast.push(voice_id(v).expect("cast voices exist"));
    }
    let threads = std::thread::available_parallelism()
        .map(|n| n.get().min(4))
        .unwrap_or(2) as i32;
    let tts = Kokoro::load(model_dir, threads)?;
    let speech = Arc::new(Speech {
        rate: tts.sample_rate(),
        total: paras.iter().map(|p| sentences(&p.text).len()).sum(),
        state: Mutex::default(),
        playhead_ms: AtomicU64::new(0),
        stop: AtomicBool::new(false),
    });
    let shared = speech.clone();
    std::thread::Builder::new()
        .name("kokoro".into())
        .spawn(move || {
            if let Err(e) = speak_all(&tts, &paras, &cast, &shared) {
                shared.state.lock().expect("speech lock").error = Some(format!("{e:#}"));
            }
            shared.state.lock().expect("speech lock").done = true;
        })?;
    Ok(speech)
}

fn speak_all(tts: &Kokoro, paras: &[Para], cast: &[i32], speech: &Speech) -> Result<()> {
    let rate = speech.rate as u64;
    let ms_of = |n: usize| n as u64 * 1000 / rate;
    let silence = |ms: u64| vec![0.0f32; (ms * rate / 1000) as usize];
    // Speakers in order of first appearance; their slot picks voice and colour.
    let mut seen: Vec<String> = Vec::new();
    let mut last: Option<&Option<String>> = None;
    for p in paras {
        let slot = p.speaker.as_ref().map(|s| {
            seen.iter().position(|x| x == s).unwrap_or_else(|| {
                seen.push(s.clone());
                seen.len() - 1
            })
        });
        let sid = cast[slot.unwrap_or(0) % cast.len()];
        let gap = match last {
            None => 0,
            Some(prev) if *prev != p.speaker => TURN_GAP_MS,
            Some(_) => PARAGRAPH_GAP_MS,
        };
        last = Some(&p.speaker);
        if gap > 0 {
            speech
                .state
                .lock()
                .expect("speech lock")
                .samples
                .extend(silence(gap));
        }
        let sents = sentences(&p.text);
        for (i, sent) in sents.iter().enumerate() {
            // Stay a while ahead of the listener, not the whole file.
            loop {
                if speech.stop.load(Ordering::Relaxed) {
                    return Ok(());
                }
                let ahead = ms_of(speech.len());
                if ahead < speech.playhead_ms.load(Ordering::Relaxed) + LOOKAHEAD_MS {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let audio = tts.speak(sent, sid, 1.0)?;
            let voiced = trim_silence(&audio);
            let mut s = speech.state.lock().expect("speech lock");
            let start = ms_of(s.samples.len());
            let end = start + ms_of(voiced.len());
            let words: Vec<&str> = sent.split_whitespace().collect();
            s.cues.push(Cue {
                start_ms: start,
                end_ms: end,
                words: spread(&words, start, end),
                speaker: p.speaker.clone(),
                slot,
            });
            s.samples.extend_from_slice(voiced);
            if i + 1 < sents.len() {
                s.samples.extend(silence(SENTENCE_GAP_MS));
            }
        }
    }
    Ok(())
}

/// `audio` without near-silent lead-in and tail, so word times line up
/// and the gaps between sentences are the ones chosen here.
pub fn trim_silence(audio: &[f32]) -> &[f32] {
    const FLOOR: f32 = 0.01;
    let first = audio.iter().position(|s| s.abs() > FLOOR).unwrap_or(0);
    let last = audio
        .iter()
        .rposition(|s| s.abs() > FLOOR)
        .map(|i| i + 1)
        .unwrap_or(audio.len());
    // Keep a few ms either side so consonants aren't clipped.
    let pad = 240;
    let first = first.saturating_sub(pad);
    let last = (last + pad).min(audio.len());
    &audio[first..last.max(first)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_scribe_markdown() {
        let md = "# tour\n\n*2026-10-04 21:43 · tour.mp3*\n\n**[00:00:00] Speaker A:** Welcome\nback.\n\n**[00:00:16]** No *one* here.\n\n---\n\n*2026-10-05*\n\nA plain note.\n";
        assert_eq!(
            paragraphs(md),
            vec![
                Para {
                    speaker: Some("Speaker A".into()),
                    text: "Welcome back.".into()
                },
                Para {
                    speaker: None,
                    text: "No one here.".into()
                },
                Para {
                    speaker: None,
                    text: "A plain note.".into()
                },
            ]
        );
    }

    #[test]
    fn splits_sentences_not_names() {
        assert_eq!(
            sentences("We use d.rymcg.tech here. Is it \"good?\" Yes and"),
            vec!["We use d.rymcg.tech here.", "Is it \"good?\"", "Yes and"]
        );
    }

    #[test]
    fn spreads_words_by_length() {
        let w = spread(&["a", "bbb"], 1000, 1600);
        assert_eq!((w[0].start_ms, w[0].end_ms), (1000, 1200));
        assert_eq!((w[1].start_ms, w[1].end_ms), (1200, 1600));
    }

    #[test]
    fn trims_quiet_ends() {
        let mut a = vec![0.0; 1000];
        a.extend(vec![0.5; 100]);
        a.extend(vec![0.0; 1000]);
        assert_eq!(trim_silence(&a).len(), 100 + 2 * 240);
    }
}

/// Needs the Kokoro model: `cargo test -p vox_scribe -- --ignored speaks_a_transcript`.
#[cfg(test)]
mod model_tests {
    use super::*;
    use std::time::Instant;

    #[test]
    #[ignore]
    fn speaks_a_transcript() {
        let dir = crate::models::kokoro_dir(&crate::models::resolve(None));
        let md = "# t\n\n**[00:00:00] Speaker A:** Hello there. How are you?\n\n**[00:00:05] Speaker B:** Fine, thanks.\n";
        let t = Instant::now();
        let speech = start(md, &dir, &[]).unwrap();
        while !speech.done() {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(speech.error(), None);
        let cues = speech.cues_from(0);
        assert_eq!(cues.len(), 3);
        assert_eq!(cues[2].slot, Some(1));
        let secs = speech.len() as f64 / speech.rate as f64;
        eprintln!("{secs:.2}s of speech in {:.2?}", t.elapsed());
        assert!(cues.windows(2).all(|w| w[0].end_ms <= w[1].start_ms));
        assert!(cues[2].end_ms <= (secs * 1000.0) as u64 + 1);
    }
}
