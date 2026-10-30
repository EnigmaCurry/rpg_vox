//! The room's mixer: every person's mic and the AI's voice in, one mix per
//! listener out, each without the listener's own voice (a mix-minus).
//!
//! Runs on its own thread on a 20 ms clock. Each tick takes one 960-sample
//! frame (48 kHz mono) from every mic buffer and from the AI's speech
//! queue, applies the strip gains, and Opus-encodes a frame for every
//! connected listener. Nothing here is a realtime audio callback, so plain
//! mutexes are fine; each lock is held for well under a millisecond.
//!
//! The AI's speech is queued as whole sentences, and the mixer counts how
//! much of each it has played. That count is what "said" means for the
//! conversation log: [`Mixer::ai_clear`] stops the AI and returns exactly
//! how far it got.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, warn};

pub const RATE: u32 = 48_000;
/// 20 ms at 48 kHz.
pub const FRAME: usize = 960;
const TICK: Duration = Duration::from_millis(20);

/// Mic audio arrives in network-sized bursts. Start playing a mic once
/// this much is buffered, and again after it runs dry.
const PRIME_FRAMES: usize = 2;
/// Past this much buffered mic audio (a stalled tab catching up), drop
/// the oldest down to [`TRIM_TO_FRAMES`] so the room never lags behind.
const MAX_BUFFER_FRAMES: usize = 10;
const TRIM_TO_FRAMES: usize = 3;

/// The AI's level while someone may be talking over it, and how fast it
/// moves there (per sample: 10 ms from full to ducked).
const DUCK_GAIN: f32 = 0.2;
const DUCK_STEP: f32 = 1.0 / 480.0;

/// Upper limit of a strip fader (about +6 dB).
pub const MAX_GAIN: f32 = 2.0;
/// The AI's strip in [`Mixer::strips`].
pub const AGENT_STRIP: &str = "@agent";

const OPUS_BITRATE: i32 = 48_000;
/// Listener frames that may wait to be sent (500 ms). Past that the
/// listener is too slow and frames are dropped.
pub const LISTENER_QUEUE: usize = 25;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Strip {
    pub gain: f32,
    pub mute: bool,
}

impl Default for Strip {
    fn default() -> Self {
        Self {
            gain: 1.0,
            mute: false,
        }
    }
}

/// How far the AI's speech has got, for the conversation log.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AiProgress {
    /// Sentences played to the end since the last report: (generation, index, samples).
    pub finished: Vec<(u64, usize, usize)>,
    /// The sentence playing now: (generation, index, samples played, samples).
    pub current: Option<(u64, usize, usize, usize)>,
    /// Nothing left to play.
    pub idle: bool,
}

struct Segment {
    generation: u64,
    index: usize,
    samples: Vec<f32>,
    pos: usize,
}

struct Ai {
    queue: VecDeque<Segment>,
    finished: Vec<(u64, usize, usize)>,
    ducked: bool,
    /// Current duck multiplier, ramped toward the target.
    duck: f32,
}

impl Default for Ai {
    /// Written out rather than derived because `duck` starts at full:
    /// a derived zero would be a queue that fades up from silence every
    /// time a language is first spoken in.
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            finished: Vec::new(),
            ducked: false,
            duck: 1.0,
        }
    }
}

impl Ai {
    fn progress(&mut self) -> AiProgress {
        AiProgress {
            finished: std::mem::take(&mut self.finished),
            current: self
                .queue
                .front()
                .map(|s| (s.generation, s.index, s.pos, s.samples.len())),
            idle: self.queue.is_empty(),
        }
    }

    /// Fill `out` from the queue, ramping the duck. Returns whether any
    /// speech was played.
    fn pull(&mut self, out: &mut [f32]) -> bool {
        let target = if self.ducked { DUCK_GAIN } else { 1.0 };
        let mut any = false;
        for o in out.iter_mut() {
            *o = 0.0;
            while let Some(seg) = self.queue.front_mut() {
                if seg.pos < seg.samples.len() {
                    break;
                }
                self.finished
                    .push((seg.generation, seg.index, seg.samples.len()));
                self.queue.pop_front();
            }
            let Some(seg) = self.queue.front_mut() else {
                continue;
            };
            if self.duck < target {
                self.duck = (self.duck + DUCK_STEP).min(target);
            } else if self.duck > target {
                self.duck = (self.duck - DUCK_STEP).max(target);
            }
            *o = seg.samples[seg.pos] * self.duck;
            seg.pos += 1;
            any = true;
        }
        // A sentence that ended exactly on the frame boundary.
        while let Some(seg) = self.queue.front() {
            if seg.pos < seg.samples.len() {
                break;
            }
            self.finished
                .push((seg.generation, seg.index, seg.samples.len()));
            self.queue.pop_front();
        }
        any
    }
}

/// One person's mic, between the WebSocket and the mixer clock.
#[derive(Default)]
struct Input {
    buf: VecDeque<f32>,
    primed: bool,
}

impl Input {
    fn pull(&mut self, out: &mut [f32]) {
        out.fill(0.0);
        if !self.primed {
            if self.buf.len() < PRIME_FRAMES * FRAME {
                return;
            }
            self.primed = true;
        }
        if self.buf.len() > MAX_BUFFER_FRAMES * FRAME {
            let drop = self.buf.len() - TRIM_TO_FRAMES * FRAME;
            self.buf.drain(..drop);
            debug!(dropped = drop, "mic buffer trimmed");
        }
        let n = self.buf.len().min(out.len());
        for (o, s) in out.iter_mut().zip(self.buf.drain(..n)) {
            *o = s;
        }
        if n < out.len() {
            self.primed = false;
        }
    }
}

struct Listener {
    id: u64,
    person: String,
    /// The language they want the AI in, spelled out in English.
    language: String,
    encoder: opus::Encoder,
    tx: mpsc::Sender<Vec<u8>>,
}

struct State {
    inputs: HashMap<String, Input>,
    strips: BTreeMap<String, Strip>,
    listeners: Vec<Listener>,
    /// The AI's speech, one queue per language the room is listening in.
    /// Every listener hears the one in their own language and no other,
    /// so these are alternatives rather than parts of a mix.
    ai: BTreeMap<String, Ai>,
    /// The language whose queue is the AI's turn as the room records it:
    /// what [`Mixer::ai_progress`] counts, and so what the conversation
    /// log and a barge-in are measured against. The others are spoken
    /// but not counted, because there is only one turn however many
    /// languages it is said in.
    canonical: String,
    /// Peak level per strip (post-fader) since the last `take_levels`.
    peaks: HashMap<String, f32>,
}

pub struct Mixer {
    state: Mutex<State>,
}

impl Mixer {
    /// `canonical` is the room's own language: the one the AI is written
    /// in, and the queue its turn is counted against.
    pub fn new(strips: BTreeMap<String, Strip>, canonical: &str) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                inputs: HashMap::new(),
                strips,
                listeners: Vec::new(),
                ai: BTreeMap::from([(canonical.to_string(), Ai::default())]),
                canonical: canonical.to_string(),
                peaks: HashMap::new(),
            }),
        })
    }

    /// Start the 20 ms clock on its own thread.
    pub fn spawn(self: &Arc<Self>) {
        let me = self.clone();
        std::thread::Builder::new()
            .name("vox-room-mixer".into())
            .spawn(move || me.run())
            .expect("spawn mixer thread");
    }

    fn run(&self) {
        let mut next = Instant::now();
        loop {
            self.tick();
            next += TICK;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            } else if now - next > TICK * 5 {
                // Fell far behind (a suspended laptop): don't burst to catch up.
                warn!(
                    behind_ms = (now - next).as_millis() as u64,
                    "mixer clock reset"
                );
                next = now;
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("mixer mutex poisoned")
    }

    /// Decoded mic audio from `person` (48 kHz mono).
    pub fn push_mic(&self, person: &str, pcm: &[f32]) {
        let mut st = self.lock();
        st.inputs
            .entry(person.to_string())
            .or_default()
            .buf
            .extend(pcm.iter().copied());
    }

    pub fn add_listener(
        &self,
        id: u64,
        person: &str,
        language: &str,
        tx: mpsc::Sender<Vec<u8>>,
    ) -> anyhow::Result<()> {
        let mut encoder = opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Audio)?;
        let _ = encoder.set_bitrate(opus::Bitrate::Bits(OPUS_BITRATE));
        self.lock().listeners.push(Listener {
            id,
            person: person.to_string(),
            language: language.to_string(),
            encoder,
            tx,
        });
        Ok(())
    }

    pub fn remove_listener(&self, id: u64) {
        self.lock().listeners.retain(|l| l.id != id);
    }

    /// The language `person`'s tabs want the AI in. Called when they
    /// choose one at the door, so an open tab doesn't have to reconnect.
    pub fn set_listener_language(&self, person: &str, language: &str) {
        let mut st = self.lock();
        for l in st.listeners.iter_mut().filter(|l| l.person == person) {
            l.language = language.to_string();
        }
    }

    /// The strip `name` is muted on the mixer.
    pub fn is_muted(&self, name: &str) -> bool {
        self.lock().strips.get(name).is_some_and(|s| s.mute)
    }

    pub fn strips(&self) -> BTreeMap<String, Strip> {
        self.lock().strips.clone()
    }

    /// Change a strip; returns the new value.
    pub fn set_strip(&self, name: &str, gain: Option<f32>, mute: Option<bool>) -> Strip {
        let mut st = self.lock();
        let s = st.strips.entry(name.to_string()).or_default();
        if let Some(g) = gain.filter(|g| g.is_finite()) {
            s.gain = g.clamp(0.0, MAX_GAIN);
        }
        if let Some(m) = mute {
            s.mute = m;
        }
        *s
    }

    /// Peak level of every strip since the last call.
    pub fn take_levels(&self) -> HashMap<String, f32> {
        std::mem::take(&mut self.lock().peaks)
    }

    /// Queue one sentence of the AI's speech in `language` (48 kHz mono).
    /// A language nobody is listening in yet gets its queue here.
    pub fn ai_push(&self, language: &str, generation: u64, index: usize, samples: Vec<f32>) {
        let mut st = self.lock();
        let ducked = st.ai.get(&st.canonical).is_some_and(|a| a.ducked);
        let ai = st.ai.entry(language.to_string()).or_default();
        if ai.queue.is_empty() && !ducked {
            ai.duck = 1.0;
        }
        ai.queue.push_back(Segment {
            generation,
            index,
            samples,
            pos: 0,
        });
    }

    /// Lower the AI while someone may be talking over it, or bring it
    /// back. Every language at once: it is one turn being interrupted.
    pub fn ai_duck(&self, ducked: bool) {
        for ai in self.lock().ai.values_mut() {
            ai.ducked = ducked;
        }
    }

    /// How far the AI's turn has got in each language it is being said
    /// in. Per language because each is read by whoever is listening in
    /// it, and they drift: a translation is a different number of
    /// syllables from the words it translates.
    pub fn ai_progress(&self) -> BTreeMap<String, AiProgress> {
        let mut st = self.lock();
        st.ai
            .iter_mut()
            .map(|(language, ai)| (language.clone(), ai.progress()))
            .collect()
    }

    /// Stop the AI now, in every language. Returns how far each got, as
    /// [`Mixer::ai_progress`].
    pub fn ai_clear(&self) -> BTreeMap<String, AiProgress> {
        let mut st = self.lock();
        let mut progress = BTreeMap::new();
        for (language, ai) in st.ai.iter_mut() {
            let mut p = ai.progress();
            ai.queue.clear();
            ai.ducked = false;
            ai.duck = 1.0;
            // Nothing is left to play, whatever was queued a moment ago.
            p.idle = true;
            progress.insert(language.clone(), p);
        }
        progress
    }

    pub(crate) fn tick(&self) {
        let mut st = self.lock();
        let st = &mut *st;
        let mut frames: Vec<(String, [f32; FRAME])> = Vec::with_capacity(st.inputs.len() + 1);
        for (person, input) in st.inputs.iter_mut() {
            let mut f = [0.0f32; FRAME];
            input.pull(&mut f);
            frames.push((person.clone(), f));
        }
        for (name, f) in frames.iter_mut() {
            let strip = st.strips.get(name.as_str()).copied().unwrap_or_default();
            let g = if strip.mute { 0.0 } else { strip.gain };
            let mut peak = 0.0f32;
            for s in f.iter_mut() {
                *s *= g;
                peak = peak.max(s.abs());
            }
            let p = st.peaks.entry(name.clone()).or_insert(0.0);
            *p = p.max(peak);
        }
        // The AI's languages, each through the one `@agent` fader: the
        // room shares an AI, so it shares a fader for it, and the level
        // shown is the loudest of whatever is being said.
        let strip = st.strips.get(AGENT_STRIP).copied().unwrap_or_default();
        let g = if strip.mute { 0.0 } else { strip.gain };
        let mut ai: BTreeMap<String, [f32; FRAME]> = BTreeMap::new();
        let mut ai_peak = 0.0f32;
        for (language, queue) in st.ai.iter_mut() {
            let mut f = [0.0f32; FRAME];
            if !queue.pull(&mut f) {
                continue;
            }
            for s in f.iter_mut() {
                *s *= g;
                ai_peak = ai_peak.max(s.abs());
            }
            ai.insert(language.clone(), f);
        }
        if !ai.is_empty() {
            let p = st.peaks.entry(AGENT_STRIP.to_string()).or_insert(0.0);
            *p = p.max(ai_peak);
        }
        if st.listeners.is_empty() {
            return;
        }
        // Everyone's mics, which everybody hears whatever they speak.
        let mut total = [0.0f32; FRAME];
        for (_, f) in &frames {
            for (t, s) in total.iter_mut().zip(f.iter()) {
                *t += s;
            }
        }
        let mut out = [0.0f32; FRAME];
        let mut packet = [0u8; 4000];
        for l in st.listeners.iter_mut() {
            let own = frames.iter().find(|(n, _)| *n == l.person).map(|(_, f)| f);
            // Their own language and no other, or nothing at all.
            //
            // There is deliberately no falling back to the room's own
            // language while a translation is still on its way. A
            // translation lands a sentence or two behind the original,
            // so a fallback plays the opening of the reply in a language
            // this listener didn't ask for and then cuts to theirs
            // mid-sentence -- which is both the one thing the feature
            // exists to avoid and a garble, since by then the two
            // queues are at different sentences. Quiet until their own
            // words arrive is the honest alternative, and the reply is
            // on screen meanwhile.
            let theirs = ai.get(&l.language);
            mix_minus(&total, own, theirs, &mut out);
            match l.encoder.encode_float(&out, &mut packet) {
                Ok(n) => {
                    // A full queue means a slow listener: drop, don't stall the room.
                    let _ = l.tx.try_send(packet[..n].to_vec());
                }
                Err(e) => warn!(listener = l.id, "opus encode failed: {e}"),
            }
        }
    }
}

/// `total` without `own` and with `ai` -- everyone in the room but
/// yourself, plus the AI in your language -- clamped to full scale.
fn mix_minus(
    total: &[f32; FRAME],
    own: Option<&[f32; FRAME]>,
    ai: Option<&[f32; FRAME]>,
    out: &mut [f32; FRAME],
) {
    for i in 0..FRAME {
        let s = total[i] - own.map_or(0.0, |o| o[i]) + ai.map_or(0.0, |a| a[i]);
        out[i] = s.clamp(-1.0, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listeners_hear_everyone_but_themselves() {
        let a = [0.25f32; FRAME];
        let b = [0.5f32; FRAME];
        let mut total = [0.0f32; FRAME];
        for i in 0..FRAME {
            total[i] = a[i] + b[i];
        }
        let mut out = [0.0f32; FRAME];
        mix_minus(&total, Some(&a), None, &mut out);
        assert!(out.iter().all(|s| (s - 0.5).abs() < 1e-6));
        mix_minus(&total, Some(&b), None, &mut out);
        assert!(out.iter().all(|s| (s - 0.25).abs() < 1e-6));
        mix_minus(&total, None, None, &mut out);
        assert!(out.iter().all(|s| (s - 0.75).abs() < 1e-6));
    }

    #[test]
    fn each_listener_hears_the_ai_in_one_language_and_no_other() {
        let english = [0.25f32; FRAME];
        let russian = [0.5f32; FRAME];
        let mics = [0.1f32; FRAME];
        let mut out = [0.0f32; FRAME];
        // The AI's languages are alternatives, never summed: whichever
        // one a listener is on is the only one they hear.
        mix_minus(&mics, None, Some(&english), &mut out);
        assert!(out.iter().all(|s| (s - 0.35).abs() < 1e-6));
        mix_minus(&mics, None, Some(&russian), &mut out);
        assert!(out.iter().all(|s| (s - 0.6).abs() < 1e-6));
        // And a mix still leaves out your own mic.
        mix_minus(&mics, Some(&mics), Some(&russian), &mut out);
        assert!(out.iter().all(|s| (s - 0.5).abs() < 1e-6));
    }

    /// What one listener's socket actually received this tick, decoded
    /// back from the Opus the mixer sent it.
    fn heard_by(rx: &mut mpsc::Receiver<Vec<u8>>, decoder: &mut opus::Decoder) -> Vec<f32> {
        let packet = rx.try_recv().expect("a frame for this listener");
        let mut pcm = vec![0.0f32; FRAME];
        let n = decoder
            .decode_float(&packet, &mut pcm, false)
            .expect("opus");
        pcm.truncate(n);
        pcm
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }

    #[test]
    fn a_listener_waiting_on_a_translation_hears_nothing_rather_than_another_language() {
        let mixer = Mixer::new(BTreeMap::new(), "English");
        let (tx, mut rx) = mpsc::channel(LISTENER_QUEUE);
        mixer.add_listener(1, "Anya", "Russian", tx).unwrap();
        let mut decoder = opus::Decoder::new(RATE, opus::Channels::Mono).unwrap();

        // The reply has begun in the room's own language; the Russian is
        // still being translated, so there is no Russian queue yet.
        mixer.ai_push("English", 1, 0, vec![0.5; 20 * FRAME]);
        mixer.tick();
        assert!(
            peak(&heard_by(&mut rx, &mut decoder)) < 0.02,
            "a listener who asked for Russian must not be handed English \
             while the translation is on its way"
        );

        // Their own words arrive, and now they hear them.
        mixer.ai_push("Russian", 1, 0, vec![0.5; 20 * FRAME]);
        mixer.tick();
        assert!(peak(&heard_by(&mut rx, &mut decoder)) > 0.1);

        // And someone on the room's own language heard it all along.
        let (tx, mut rx2) = mpsc::channel(LISTENER_QUEUE);
        mixer.add_listener(2, "Ryan", "English", tx).unwrap();
        let mut d2 = opus::Decoder::new(RATE, opus::Channels::Mono).unwrap();
        mixer.tick();
        assert!(peak(&heard_by(&mut rx2, &mut d2)) > 0.1);
    }

    #[test]
    fn each_language_of_the_ai_s_turn_is_counted_on_its_own() {
        let mixer = Mixer::new(BTreeMap::new(), "English");
        // Half a frame of English, two frames of Russian: the same
        // sentence of the same turn, said at different lengths. Whoever
        // is reading along in Russian must be told how far the Russian
        // has got, not how far the English has.
        mixer.ai_push("English", 7, 0, vec![0.5; FRAME / 2]);
        mixer.ai_push("Russian", 7, 0, vec![0.5; 2 * FRAME]);
        mixer.tick();

        let p = mixer.ai_progress();
        assert_eq!(
            p["English"].finished,
            vec![(7, 0, FRAME / 2)],
            "the English sentence played to the end"
        );
        assert!(p["English"].idle, "and nothing of it is left");
        assert_eq!(
            p["Russian"].current,
            Some((7, 0, FRAME, 2 * FRAME)),
            "the Russian is half said"
        );
        assert!(!p["Russian"].idle);

        mixer.tick();
        let p = mixer.ai_progress();
        assert!(p["English"].finished.is_empty());
        assert_eq!(p["Russian"].finished, vec![(7, 0, 2 * FRAME)]);
        assert!(p["Russian"].idle, "now both are done");
    }

    #[test]
    fn stopping_the_ai_stops_every_language_at_once() {
        let mixer = Mixer::new(BTreeMap::new(), "English");
        mixer.ai_push("English", 3, 0, vec![0.5; 10 * FRAME]);
        mixer.ai_push("Russian", 3, 0, vec![0.5; 10 * FRAME]);
        mixer.tick();

        let p = mixer.ai_clear();
        for language in ["English", "Russian"] {
            assert_eq!(
                p[language].current,
                Some((3, 0, FRAME, 10 * FRAME)),
                "{language} cut one frame in"
            );
            assert!(p[language].idle, "{language} has nothing left to play");
        }
        // Nothing of either language is left to play: an interruption
        // ends the turn, not just the one rendition of it.
        mixer.tick();
        for (_, after) in mixer.ai_progress() {
            assert!(after.idle && after.finished.is_empty() && after.current.is_none());
        }
    }

    #[test]
    fn mic_waits_to_prime_then_plays_and_reprimes_after_running_dry() {
        let mut input = Input::default();
        let mut out = [0.0f32; FRAME];
        input.buf.extend(std::iter::repeat_n(1.0, FRAME));
        input.pull(&mut out);
        assert!(
            out.iter().all(|s| *s == 0.0),
            "one frame isn't enough to start"
        );
        input.buf.extend(std::iter::repeat_n(1.0, FRAME));
        input.pull(&mut out);
        assert!(out.iter().all(|s| *s == 1.0));
        input.pull(&mut out);
        assert!(out.iter().all(|s| *s == 1.0));
        input.pull(&mut out);
        assert!(!input.primed, "ran dry");
    }

    #[test]
    fn a_backed_up_mic_is_trimmed() {
        let mut input = Input::default();
        input.buf.extend(std::iter::repeat_n(0.1, 20 * FRAME));
        let mut out = [0.0f32; FRAME];
        input.pull(&mut out);
        assert_eq!(input.buf.len(), (TRIM_TO_FRAMES - 1) * FRAME);
    }

    #[test]
    fn counts_what_the_ai_has_said() {
        let mut ai = Ai::default();
        ai.queue.push_back(Segment {
            generation: 7,
            index: 0,
            samples: vec![0.5; FRAME + 100],
            pos: 0,
        });
        ai.queue.push_back(Segment {
            generation: 7,
            index: 1,
            samples: vec![0.5; 3 * FRAME],
            pos: 0,
        });
        let mut out = [0.0f32; FRAME];
        assert!(ai.pull(&mut out));
        let p = ai.progress();
        assert!(p.finished.is_empty());
        assert_eq!(p.current, Some((7, 0, FRAME, FRAME + 100)));
        ai.pull(&mut out);
        let p = ai.progress();
        assert_eq!(p.finished, vec![(7, 0, FRAME + 100)]);
        assert_eq!(p.current, Some((7, 1, FRAME - 100, 3 * FRAME)));
        assert!(!p.idle);
    }

    #[test]
    fn ducking_ramps_instead_of_clicking() {
        let mut ai = Ai::default();
        ai.queue.push_back(Segment {
            generation: 1,
            index: 0,
            samples: vec![1.0; 4 * FRAME],
            pos: 0,
        });
        ai.ducked = true;
        let mut out = [0.0f32; FRAME];
        ai.pull(&mut out);
        assert!(out[0] > 0.99);
        assert!(out
            .windows(2)
            .all(|w| (w[0] - w[1]).abs() <= DUCK_STEP + 1e-6));
        assert!((out[FRAME - 1] - DUCK_GAIN).abs() < 1e-6);
    }
}
