//! Speaks text as it arrives, for `--chat`: one thread turns each
//! sentence into audio with Kokoro, another plays it and reports which
//! word is being heard (times spread over each sentence by word length,
//! as in [`crate::speak`]). Playback can be paused, and [`Voice::stop`]
//! drops everything not yet heard.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result};
use crossbeam_channel::{Receiver, Sender};
use tracing::warn;
use vox_audio::playback::Output;
use vox_audio::resample::Linear;
use vox_transcribe::tts::Kokoro;

/// Silence between sentences.
const GAP_MS: u64 = 150;

#[derive(Default)]
struct Shared {
    /// Bumped by [`Voice::stop`]; work from an older generation is dropped.
    generation: AtomicU64,
    /// Texts handed to [`Voice::say`] and not yet fully played.
    pending: AtomicUsize,
    /// Audio is queued or still coming out of the speaker.
    sounding: AtomicBool,
    paused: AtomicBool,
    /// Kokoro speaker id; [`Voice::set_voice`] changes it.
    sid: AtomicI32,
    /// (key, index of the word being heard among all the words said
    /// under that key).
    heard: Mutex<Option<(i64, usize)>>,
}

/// One sentence's audio.
struct Clip {
    key: i64,
    /// Index of its first word under `key`.
    first_word: usize,
    samples: Vec<f32>,
    /// Where each word ends, in ms from the start.
    word_ends_ms: Vec<u64>,
}

enum Audio {
    Clip(u64, Clip),
    /// Everything for one [`Voice::say`] has been queued.
    End(u64),
}

/// A clip handed to the output: where it starts in frames pushed since
/// the last flush, and its words' ends in output frames.
struct Mark {
    start: u64,
    key: i64,
    first_word: usize,
    ends: Vec<u64>,
}

pub struct Voice {
    jobs: Sender<(u64, i64, String)>,
    shared: Arc<Shared>,
}

impl Voice {
    /// Speak with voice `sid` of `tts`.
    pub fn new(tts: Kokoro, sid: i32) -> Result<Self> {
        let shared = Arc::new(Shared::default());
        shared.sid.store(sid, Ordering::SeqCst);
        let (jobs, job_rx) = crossbeam_channel::unbounded::<(u64, i64, String)>();
        let (audio_tx, audio_rx) = crossbeam_channel::unbounded::<Audio>();
        let rate = tts.sample_rate();
        {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("voice-synth".into())
                .spawn(move || synth(tts, &shared, job_rx, audio_tx))?;
        }
        // The output is opened on its own thread, which keeps it.
        let (ready_tx, ready_rx) = crossbeam_channel::bounded(1);
        {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("voice-play".into())
                .spawn(move || match Output::open().context("open audio output") {
                    Ok(out) => {
                        let _ = ready_tx.send(Ok(()));
                        play(out, rate, &shared, audio_rx);
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                })?;
        }
        ready_rx.recv()??;
        Ok(Self { jobs, shared })
    }

    /// Queue `text` to be spoken after anything already queued. Its
    /// words are counted on from earlier text with the same `key`.
    pub fn say(&self, key: i64, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let generation = self.shared.generation.load(Ordering::SeqCst);
        self.shared.pending.fetch_add(1, Ordering::SeqCst);
        let _ = self.jobs.send((generation, key, text.to_string()));
    }

    /// The word being heard now: (key, its index under that key).
    pub fn heard(&self) -> Option<(i64, usize)> {
        *self.shared.heard.lock().expect("heard lock")
    }

    /// Something is queued, being made, or playing (paused or not).
    pub fn busy(&self) -> bool {
        self.shared.pending.load(Ordering::SeqCst) > 0
            || self.shared.sounding.load(Ordering::SeqCst)
    }

    pub fn set_paused(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::SeqCst);
    }

    /// Speak with Kokoro voice `sid` from the next sentence on.
    pub fn set_voice(&self, sid: i32) {
        self.shared.sid.store(sid, Ordering::SeqCst);
    }

    /// Drop everything not yet heard.
    pub fn stop(&self) {
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        self.shared.pending.store(0, Ordering::SeqCst);
        self.shared.paused.store(false, Ordering::SeqCst);
    }
}

fn synth(tts: Kokoro, shared: &Shared, jobs: Receiver<(u64, i64, String)>, out: Sender<Audio>) {
    let current = |g: u64| shared.generation.load(Ordering::SeqCst) == g;
    let rate = tts.sample_rate() as u64;
    let gap = vec![0.0f32; (GAP_MS * rate / 1000) as usize];
    // Words said so far under each key.
    let mut counts: std::collections::HashMap<i64, usize> = Default::default();
    let mut counted_for = shared.generation.load(Ordering::SeqCst);
    for (generation, key, text) in jobs {
        // After a stop, a key's words count from 0 again.
        if generation != counted_for {
            counts.clear();
            counted_for = generation;
        }
        for sentence in crate::speak::sentences(&text) {
            if !current(generation) {
                break;
            }
            let words: Vec<&str> = sentence.split_whitespace().collect();
            let first_word = *counts.get(&key).unwrap_or(&0);
            counts.insert(key, first_word + words.len());
            let sid = shared.sid.load(Ordering::SeqCst);
            match tts.speak(&sentence, sid, 1.0) {
                Ok(audio) if current(generation) => {
                    let mut samples = crate::speak::trim_silence(&audio).to_vec();
                    let voiced_ms = samples.len() as u64 * 1000 / rate;
                    let word_ends_ms = crate::speak::spread(&words, 0, voiced_ms)
                        .iter()
                        .map(|w| w.end_ms)
                        .collect();
                    samples.extend_from_slice(&gap);
                    let clip = Clip {
                        key,
                        first_word,
                        samples,
                        word_ends_ms,
                    };
                    if out.send(Audio::Clip(generation, clip)).is_err() {
                        return;
                    }
                }
                Ok(_) => break,
                Err(e) => warn!("speaking {sentence:?} failed: {e:#}"),
            }
        }
        let _ = out.send(Audio::End(generation));
    }
}

fn play(mut out: Output, rate: u32, shared: &Shared, audio: Receiver<Audio>) {
    let mut resampler = Linear::new(rate, out.sample_rate);
    let mut queue: VecDeque<Audio> = VecDeque::new();
    let mut buf: Vec<f32> = Vec::new();
    let mut generation = shared.generation.load(Ordering::SeqCst);
    let mut was_paused = false;
    // Frames pushed since the last flush, and the clips among them.
    let mut pushed: u64 = 0;
    let mut marks: VecDeque<Mark> = VecDeque::new();
    let out_rate = out.sample_rate as u64;
    let frames = |ms: u64| ms * out_rate / 1000;
    loop {
        match audio.recv_timeout(Duration::from_millis(10)) {
            Ok(a) => queue.push_back(a),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
        }
        queue.extend(audio.try_iter());
        let now = shared.generation.load(Ordering::SeqCst);
        if now != generation {
            generation = now;
            out.flush();
            resampler.reset();
            buf.clear();
            pushed = 0;
            marks.clear();
        }
        queue.retain(|a| match a {
            Audio::Clip(g, _) | Audio::End(g) => *g == generation,
        });
        let paused = shared.paused.load(Ordering::SeqCst);
        if paused != was_paused {
            out.set_paused(paused);
            was_paused = paused;
        }
        while !paused && out.free() > 0 {
            if buf.is_empty() {
                match queue.pop_front() {
                    Some(Audio::Clip(_, c)) => {
                        marks.push_back(Mark {
                            start: pushed,
                            key: c.key,
                            first_word: c.first_word,
                            ends: c.word_ends_ms.iter().map(|&ms| frames(ms)).collect(),
                        });
                        resampler.process(&c.samples, &mut buf);
                    }
                    Some(Audio::End(_)) => {
                        let _ =
                            shared
                                .pending
                                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                                    n.checked_sub(1)
                                });
                        continue;
                    }
                    None => break,
                }
            }
            let n = out.push(&buf);
            buf.drain(..n);
            pushed += n as u64;
            if n == 0 {
                break;
            }
        }
        let sounding = !buf.is_empty()
            || queue.iter().any(|a| matches!(a, Audio::Clip(..)))
            || out.queued() > 0;
        shared.sounding.store(sounding, Ordering::SeqCst);
        // The clip being heard is the last one started.
        let played = out.played();
        while marks.len() > 1 && marks[1].start <= played {
            marks.pop_front();
        }
        let heard = marks.front().filter(|_| sounding).map(|m| {
            let into = played.saturating_sub(m.start);
            let i = m.ends.partition_point(|&e| e <= into);
            (m.key, m.first_word + i.min(m.ends.len().saturating_sub(1)))
        });
        *shared.heard.lock().expect("heard lock") = heard;
    }
}

/// Plays audio and needs the Kokoro model:
/// `cargo test -p vox_scribe -- --ignored hears_words_in_order`.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn hears_words_in_order() {
        let dir = crate::models::kokoro_dir(&crate::models::resolve(None));
        let voice = Voice::new(Kokoro::load(&dir, 4).unwrap(), 3).unwrap();
        voice.say(7, "One two three.");
        voice.say(7, "Four five six seven.");
        let mut seen: Vec<usize> = Vec::new();
        let t = std::time::Instant::now();
        while voice.busy() && t.elapsed() < Duration::from_secs(20) {
            if let Some((key, i)) = voice.heard() {
                assert_eq!(key, 7);
                if seen.last() != Some(&i) {
                    seen.push(i);
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        eprintln!("heard {seen:?}");
        assert!(seen.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(seen.first(), Some(&0));
        assert_eq!(seen.last(), Some(&6));
        assert_eq!(voice.heard(), None);

        // Stopped and said again (space in --chat): counts from 0.
        voice.say(7, "Eight nine ten.");
        std::thread::sleep(Duration::from_millis(1500));
        voice.stop();
        voice.say(7, "Again from the top.");
        let mut first = None;
        while voice.busy() && first.is_none() {
            first = voice.heard();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(first, Some((7, 0)));
    }
}
