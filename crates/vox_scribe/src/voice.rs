//! Speaks text as it arrives, for `--chat`: one thread turns each
//! sentence into audio with Kokoro, another plays it. Playback can be
//! paused, and [`Voice::stop`] drops everything not yet heard.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
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
}

enum Audio {
    Samples(u64, Vec<f32>),
    /// Everything for one [`Voice::say`] has been queued.
    End(u64),
}

pub struct Voice {
    jobs: Sender<(u64, String)>,
    shared: Arc<Shared>,
}

impl Voice {
    /// Speak with voice `sid` of `tts`.
    pub fn new(tts: Kokoro, sid: i32) -> Result<Self> {
        let shared = Arc::new(Shared::default());
        let (jobs, job_rx) = crossbeam_channel::unbounded::<(u64, String)>();
        let (audio_tx, audio_rx) = crossbeam_channel::unbounded::<Audio>();
        let rate = tts.sample_rate();
        {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("voice-synth".into())
                .spawn(move || synth(tts, sid, &shared, job_rx, audio_tx))?;
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

    /// Queue `text` to be spoken after anything already queued.
    pub fn say(&self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let generation = self.shared.generation.load(Ordering::SeqCst);
        self.shared.pending.fetch_add(1, Ordering::SeqCst);
        let _ = self.jobs.send((generation, text.to_string()));
    }

    /// Something is queued, being made, or playing (paused or not).
    pub fn busy(&self) -> bool {
        self.shared.pending.load(Ordering::SeqCst) > 0
            || self.shared.sounding.load(Ordering::SeqCst)
    }

    pub fn set_paused(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::SeqCst);
    }

    /// Drop everything not yet heard.
    pub fn stop(&self) {
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        self.shared.pending.store(0, Ordering::SeqCst);
        self.shared.paused.store(false, Ordering::SeqCst);
    }
}

fn synth(
    tts: Kokoro,
    sid: i32,
    shared: &Shared,
    jobs: Receiver<(u64, String)>,
    out: Sender<Audio>,
) {
    let current = |g: u64| shared.generation.load(Ordering::SeqCst) == g;
    let gap = vec![0.0f32; (GAP_MS * tts.sample_rate() as u64 / 1000) as usize];
    for (generation, text) in jobs {
        for sentence in crate::speak::sentences(&text) {
            if !current(generation) {
                break;
            }
            match tts.speak(&sentence, sid, 1.0) {
                Ok(audio) if current(generation) => {
                    let mut samples = crate::speak::trim_silence(&audio).to_vec();
                    samples.extend_from_slice(&gap);
                    if out.send(Audio::Samples(generation, samples)).is_err() {
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
        }
        queue.retain(|a| match a {
            Audio::Samples(g, _) | Audio::End(g) => *g == generation,
        });
        let paused = shared.paused.load(Ordering::SeqCst);
        if paused != was_paused {
            out.set_paused(paused);
            was_paused = paused;
        }
        while !paused && out.free() > 0 {
            if buf.is_empty() {
                match queue.pop_front() {
                    Some(Audio::Samples(_, s)) => resampler.process(&s, &mut buf),
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
            if n == 0 {
                break;
            }
        }
        let sounding = !buf.is_empty()
            || queue.iter().any(|a| matches!(a, Audio::Samples(..)))
            || out.queued() > 0;
        shared.sounding.store(sounding, Ordering::SeqCst);
    }
}
