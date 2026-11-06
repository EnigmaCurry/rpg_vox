//! The room's mixer: every person's mic and the AI's voice in, one mix per
//! listener out, each without the listener's own voice (a mix-minus).
//!
//! One mixer per room, all of them ticked by the one 20 ms clock in
//! [`crate::rooms`]. Each tick takes one 960-sample frame (48 kHz mono)
//! from every mic buffer and from the AI's speech queue, applies the
//! strip gains, and Opus-encodes a frame for every connected listener --
//! and returns at once if nobody is listening, which is most rooms most
//! of the time. Nothing here is a realtime audio callback, so plain
//! mutexes are fine; each lock is held for well under a millisecond.
//!
//! A recording of the room is taken here too, when one is being made:
//! the sum before any of it is encoded for anybody, which is the room
//! rather than one of the headphones. See [`Mixer::record_to`].
//!
//! The AI's speech is queued as whole sentences, and the mixer counts how
//! much of each it has played. That count is what "said" means for the
//! conversation log: [`Mixer::ai_clear`] stops the AI and returns exactly
//! how far it got.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, warn};

pub const RATE: u32 = 48_000;
/// 20 ms at 48 kHz.
pub const FRAME: usize = 960;
/// Mic audio arrives in network-sized bursts. Start playing a mic once
/// this much is buffered, and again after it runs dry.
const PRIME_FRAMES: usize = 2;
/// Past this much buffered mic audio (a stalled tab catching up), drop
/// the oldest down to [`TRIM_TO_FRAMES`] so the room never lags behind.
const MAX_BUFFER_FRAMES: usize = 10;
const TRIM_TO_FRAMES: usize = 3;

/// The AI's level while someone may be talking over it, and how fast it
/// moves there (per sample: 10 ms from full to ducked). The same figure
/// is what the floor drops to under an interpreter at unity gain --
/// about 14 dB -- but there it follows the fader: see [`floor_under`].
const DUCK_GAIN: f32 = 0.2;
const DUCK_STEP: f32 = 1.0 / 480.0;

/// Upper limit of a strip fader (about +6 dB).
pub const MAX_GAIN: f32 = 2.0;
/// The AI's strip in [`Mixer::strips`].
pub const AGENT_STRIP: &str = "@agent";
/// The interpreters' strip. Pulled all the way down it is off: nothing
/// is spoken and nothing ducks, though what people say is still put
/// into the other languages to be read.
///
/// Not a strip in [`Mixer::strips`], unlike every other name here: an
/// interpretation is heard by the people listening in one language and
/// by nobody else, so how loud it is, is theirs -- see
/// [`Listener::interp`]. This is the name their meter arrives under.
pub const INTERP_STRIP: &str = "@interpreter";

/// Where the interpreter speaking `language` reports its level. One per
/// language, because the one fader is gone: a listener watches the
/// meter for the language they are hearing.
///
/// Read before the fader, on purpose. The fader is the listener's own
/// and the meter is the room's, so there is no one level this could be
/// read after -- and a meter that moves while you have the voice turned
/// down is the useful one: it says the thing you turned off is saying
/// something.
pub fn interp_level(language: &str) -> String {
    format!("{INTERP_STRIP}:{language}")
}

/// How far the floor drops under an interpreter at `gain`.
///
/// The louder the interpreter, the further down the voices it is
/// speaking over: in dB it is a straight line, [`DUCK_GAIN`]'s 14 dB for
/// every unit of fader. At unity that is the same duck as the AI's; at
/// nothing it is no duck at all, which is what makes pulling the fader
/// down turn the whole thing off rather than leave a hole in the room.
fn floor_under(gain: f32) -> f32 {
    DUCK_GAIN.powf(gain.clamp(0.0, MAX_GAIN))
}

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

/// How far a queue of speech has got, for the conversation log and for
/// everyone's screen.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackProgress {
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

/// One voice's queue: the AI in one language, or the interpreter in one
/// language. Both are heard by whoever picked that language and by
/// nobody else, so both are a queue of their own.
struct Track {
    queue: VecDeque<Segment>,
    finished: Vec<(u64, usize, usize)>,
    ducked: bool,
    /// Current duck multiplier, ramped toward the target.
    duck: f32,
}

impl Default for Track {
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

impl Track {
    fn progress(&mut self) -> TrackProgress {
        TrackProgress {
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
    /// How loud the interpreter is **for them**.
    ///
    /// An interpretation is heard by whoever is listening in that
    /// language and by nobody else, so turning it down is not a thing
    /// anyone does to the room -- it is a thing you do to what is
    /// coming out of your own headphones, like the volume. Every other
    /// fader here is the room's and is shared on purpose; this one is
    /// not, and is sent up by each tab for itself.
    interp: Strip,
    encoder: opus::Encoder,
    tx: mpsc::Sender<Vec<u8>>,
}

struct State {
    inputs: HashMap<String, Input>,
    strips: BTreeMap<String, Strip>,
    listeners: Vec<Listener>,
    /// What the AIs are saying: one queue per bot per language the room
    /// is listening in, keyed `(bot's strip, language)`.
    ///
    /// Across languages these are alternatives — every listener hears
    /// their own and no other — and across bots they are parts of a
    /// mix, because two bots talking are two voices in the room and not
    /// two versions of one. Each goes through its own fader.
    ai: BTreeMap<(String, String), Track>,
    /// What the people who don't speak the floor's language are hearing
    /// in place of understanding it: one queue per language being
    /// interpreted into, the same shape as the AI's.
    interp: BTreeMap<String, Track>,
    /// How much of the floor each listener is hearing, ramped per
    /// sample: the voices being interpreted drop away under the voice
    /// interpreting them, and come back when it stops. By listener and
    /// not by language, because how far it drops follows how loud they
    /// have their own interpreter.
    floor: HashMap<u64, f32>,
    /// The language whose queue is the AI's turn as the room records it:
    /// what [`Mixer::ai_progress`] counts, and so what the conversation
    /// log and a barge-in are measured against. The others are spoken
    /// but not counted, because there is only one turn however many
    /// languages it is said in.
    canonical: String,
    /// Peak level per strip (post-fader) since the last `take_levels`.
    peaks: HashMap<String, f32>,
    /// Where a recording of the room is being taken, while one is: see
    /// [`Mixer::record_to`].
    record: Option<crossbeam_channel::Sender<Vec<f32>>>,
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
                ai: BTreeMap::new(),
                interp: BTreeMap::new(),
                floor: HashMap::new(),
                canonical: canonical.to_string(),
                peaks: HashMap::new(),
                record: None,
            }),
        })
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

    /// Let go of whatever of `person`'s voice is still buffered. Called
    /// when their last tab here has gone: what they were saying as they
    /// left is not to be played when they come back.
    pub fn drop_mic(&self, person: &str) {
        let mut st = self.lock();
        st.inputs.remove(person);
        st.peaks.remove(person);
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
            // Where every tab starts; it says so itself if it wants
            // something else, as it does for its mic and its mute.
            interp: Strip::default(),
            encoder,
            tx,
        });
        Ok(())
    }

    /// Start handing the room's own mix to `tx`, or stop (`None`).
    ///
    /// WHAT IS IN IT: every microphone at its fader, summed, plus the
    /// AI in the room's own language -- the language its turn is
    /// recorded in, see `State::canonical`. PCM, in the same 20 ms
    /// frames everything else here works in, before any of it has been
    /// encoded for anybody.
    ///
    /// WHAT IS NOT, and why this is not just another listener: a
    /// listener hears the room *minus their own voice*, with the AI in
    /// whichever language they read and their own choice of how loud
    /// the interpreter is. Recording through one of those would be
    /// recording a pair of headphones rather than the room -- and it
    /// would mean decoding Opus back to PCM to get a file out of it. An
    /// interpretation is left out for the same reason: it is one
    /// listener's way in to what was already said, and all of them at
    /// once is nobody's. The transcript beside the clip has every
    /// language in it.
    ///
    /// Frames are dropped rather than waited for if the writer falls
    /// behind, as a slow listener's are: the room's clock is not the
    /// recorder's to hold up.
    pub fn record_to(&self, tx: Option<crossbeam_channel::Sender<Vec<f32>>>) {
        self.lock().record = tx;
    }

    pub fn remove_listener(&self, id: u64) {
        let mut st = self.lock();
        st.listeners.retain(|l| l.id != id);
        st.floor.remove(&id);
    }

    /// How loud one tab wants the interpreter. Nobody else's mix
    /// changes; returns what it is now.
    pub fn set_listener_interp(&self, id: u64, gain: Option<f32>, mute: Option<bool>) -> Strip {
        let mut st = self.lock();
        let Some(l) = st.listeners.iter_mut().find(|l| l.id == id) else {
            return Strip::default();
        };
        if let Some(g) = gain {
            l.interp.gain = g.clamp(0.0, MAX_GAIN);
        }
        if let Some(m) = mute {
            l.interp.mute = m;
        }
        l.interp
    }

    /// Whether anybody listening in `language` would actually hear an
    /// interpretation. Nobody with it turned up means nothing worth
    /// paying a voice to say -- the words still get written down.
    pub fn interp_wanted(&self, language: &str) -> bool {
        self.lock()
            .listeners
            .iter()
            .any(|l| l.language == language && !l.interp.mute && l.interp.gain > 0.0)
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

    /// The room calls `was` `now`. The fader is pointed at a person, so
    /// it goes with them -- turned down and renamed is still turned
    /// down. True when there was one to move, which is what tells the
    /// room to write the faders out again.
    ///
    /// Their meter and their listeners move with it, for the moment
    /// before their tabs come back under the new name: a level nobody
    /// claims shows on the roster, and a listener is how the mixer
    /// knows whose voice to keep out of whose headphones. Their mic is
    /// let go of rather than moved -- see [`Mixer::drop_mic`].
    pub fn rename(&self, was: &str, now: &str) -> bool {
        let mut st = self.lock();
        if let Some(peak) = st.peaks.remove(was) {
            st.peaks.insert(now.to_string(), peak);
        }
        for l in st.listeners.iter_mut().filter(|l| l.person == was) {
            l.person = now.to_string();
        }
        match st.strips.remove(was) {
            Some(strip) => {
                st.strips.insert(now.to_string(), strip);
                true
            }
            None => false,
        }
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

    /// Queue one sentence of `bot`'s speech in `language` (48 kHz mono).
    /// A language nobody is listening in yet gets its queue here.
    pub fn ai_push(
        &self,
        bot: &str,
        language: &str,
        generation: u64,
        index: usize,
        samples: Vec<f32>,
    ) {
        let mut st = self.lock();
        let ducked = st
            .ai
            .get(&(bot.to_string(), st.canonical.clone()))
            .is_some_and(|a| a.ducked);
        let ai = st
            .ai
            .entry((bot.to_string(), language.to_string()))
            .or_default();
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
    ///
    /// Idempotent, and meant to be called with what the room wants
    /// rather than only when the answer changes. It is the only thing
    /// that knows whether a voice is stepped back, so the room asking
    /// for the same answer twice costs a lock and a walk over a handful
    /// of tracks -- and the room keeping its own copy of the answer
    /// cost a show that played quietly to the end. See `Room::barge_in`.
    pub fn ai_duck(&self, ducked: bool) {
        for ai in self.lock().ai.values_mut() {
            ai.ducked = ducked;
        }
    }

    /// Whether anything the AI is saying is stepped back right now.
    ///
    /// For the tests, and only them -- which is the whole point. The
    /// room used to keep its own copy of this and gate its writes on
    /// it; the copy could disagree with the truth, and did. Nothing
    /// outside a test has any business asking: the room says what it
    /// wants and the mixer is what knows.
    #[cfg(test)]
    pub fn ai_ducked(&self) -> bool {
        self.lock().ai.values().any(|ai| ai.ducked)
    }

    /// How far `bot`'s turn has got in each language it is being said
    /// in. Per language because each is read by whoever is listening in
    /// it, and they drift: a translation is a different number of
    /// syllables from the words it translates.
    pub fn ai_progress(&self, bot: &str) -> BTreeMap<String, TrackProgress> {
        let mut st = self.lock();
        st.ai
            .iter_mut()
            .filter(|((b, _), _)| b == bot)
            .map(|((_, language), ai)| (language.clone(), ai.progress()))
            .collect()
    }

    /// Stop `bot` now, in every language. Returns how far each got, as
    /// [`Mixer::ai_progress`].
    pub fn ai_clear(&self, bot: &str) -> BTreeMap<String, TrackProgress> {
        let mut st = self.lock();
        let mut progress = BTreeMap::new();
        for ((_, language), ai) in st.ai.iter_mut().filter(|((b, _), _)| b == bot) {
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

    /// Queue one utterance of interpreted speech in `language`. `stream`
    /// names what is being interpreted -- one person's run of talk -- so
    /// the room can follow it back to the line on screen; `index` is the
    /// utterance's place in it.
    pub fn interp_push(&self, language: &str, stream: u64, index: usize, samples: Vec<f32>) {
        let mut st = self.lock();
        st.interp
            .entry(language.to_string())
            .or_default()
            .queue
            .push_back(Segment {
                generation: stream,
                index,
                samples,
                pos: 0,
            });
    }

    /// How much interpreted speech is waiting to be heard in `language`,
    /// in samples. What is queued is how far behind the room an
    /// interpreter already is, and the room stops giving it more to say
    /// rather than let that grow without end.
    pub fn interp_backlog(&self, language: &str) -> usize {
        self.lock().interp.get(language).map_or(0, |t| {
            t.queue
                .iter()
                .map(|s| s.samples.len().saturating_sub(s.pos))
                .sum()
        })
    }

    /// How far the interpreter has got in each language it is speaking,
    /// so whoever is being interpreted can see when they have been heard.
    pub fn interp_progress(&self) -> BTreeMap<String, TrackProgress> {
        let mut st = self.lock();
        st.interp
            .iter_mut()
            .map(|(language, t)| (language.clone(), t.progress()))
            .collect()
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
        // Every bot through its own fader, and what a listener hears in
        // their language is all of them together: two bots talking are
        // two voices in the room, not two versions of one. Each reports
        // its own level, because each has its own strip to show it on.
        let mut ai: BTreeMap<String, [f32; FRAME]> = BTreeMap::new();
        let mut said: BTreeMap<String, f32> = BTreeMap::new();
        for ((bot, language), queue) in st.ai.iter_mut() {
            let strip = st.strips.get(bot.as_str()).copied().unwrap_or_default();
            let g = if strip.mute { 0.0 } else { strip.gain };
            let mut f = [0.0f32; FRAME];
            if !queue.pull(&mut f) {
                continue;
            }
            let into = ai.entry(language.clone()).or_insert([0.0f32; FRAME]);
            let mut peak = 0.0f32;
            for (t, s) in into.iter_mut().zip(f.iter()) {
                let s = s * g;
                *t += s;
                peak = peak.max(s.abs());
            }
            let p = said.entry(bot.clone()).or_insert(0.0);
            *p = p.max(peak);
        }
        for (bot, peak) in said {
            let p = st.peaks.entry(bot).or_insert(0.0);
            *p = p.max(peak);
        }
        // The interpreters, one voice per language, held back while the
        // AI is speaking in the same language: both are in that
        // listener's language, and two voices at once is neither of
        // them. The AI's turn is short, and what is waiting keeps its
        // place in the queue.
        //
        // Pulled at full here and faded per listener below, because how
        // loud your interpreter is, is yours. The queues drain whatever
        // anybody has their fader at, so that the words on screen keep
        // pace with a voice somebody may be bringing back up -- it is
        // the mix they leave, not the queue.
        let mut interp: BTreeMap<String, [f32; FRAME]> = BTreeMap::new();
        for (language, queue) in st.interp.iter_mut() {
            if st
                .ai
                .iter()
                .any(|((_, l), a)| l == language && !a.queue.is_empty())
            {
                continue;
            }
            let mut f = [0.0f32; FRAME];
            if !queue.pull(&mut f) {
                continue;
            }
            let peak = f.iter().fold(0.0f32, |p, s| p.max(s.abs()));
            let p = st.peaks.entry(interp_level(language)).or_insert(0.0);
            *p = p.max(peak);
            interp.insert(language.clone(), f);
        }
        // Everyone's mics, which everybody hears whatever they speak.
        let mut total = [0.0f32; FRAME];
        for (_, f) in &frames {
            for (t, s) in total.iter_mut().zip(f.iter()) {
                *t += s;
            }
        }
        // A recording of the room, if one is being made. Before the
        // early return below and not after it: a room with nobody
        // listening can still have the AI talking in it, and a
        // recording that skipped those frames would come out shorter
        // than the stretch of transcript beside it.
        if let Some(tx) = &st.record {
            let mut f = total;
            if let Some(ai) = ai.get(&st.canonical) {
                for (t, s) in f.iter_mut().zip(ai.iter()) {
                    *t += s;
                }
            }
            for s in f.iter_mut() {
                *s = s.clamp(-1.0, 1.0);
            }
            let _ = tx.try_send(f.to_vec());
        }
        if st.listeners.is_empty() {
            return;
        }
        // What language each person is in, which is what says whether
        // they are being interpreted for a given listener.
        let mut speaks: HashMap<String, String> = HashMap::new();
        for l in &st.listeners {
            speaks.insert(l.person.clone(), l.language.clone());
        }
        // Only the voices being interpreted duck under an interpreter:
        // somebody speaking your own language is understood, and nothing
        // is being said over them. Per language, since that is what says
        // who is being interpreted; how far they drop is per listener,
        // below. A room that shares one language never interprets, so
        // this is nothing to build.
        let ducking = !interp.is_empty() || st.floor.values().any(|g| *g != 1.0);
        let mut foreign: BTreeMap<String, [f32; FRAME]> = BTreeMap::new();
        if ducking {
            let languages: BTreeSet<String> =
                st.listeners.iter().map(|l| l.language.clone()).collect();
            for language in languages {
                let mut f = [0.0f32; FRAME];
                for (person, frame) in &frames {
                    let theirs = speaks
                        .get(person)
                        .map(String::as_str)
                        .unwrap_or(&st.canonical);
                    if theirs == language {
                        continue;
                    }
                    for (t, s) in f.iter_mut().zip(frame.iter()) {
                        *t += s;
                    }
                }
                foreign.insert(language, f);
            }
        }
        let mut out = [0.0f32; FRAME];
        let mut packet = [0u8; 4000];
        // One listener's interpreter at their own level, and how far the
        // floor has dropped under it for them. Reused each time round.
        let mut theirs_interp = [0.0f32; FRAME];
        let mut env = [0.0f32; FRAME];
        for l in st.listeners.iter_mut() {
            let own = frames.iter().find(|(n, _)| *n == l.person).map(|(_, f)| f);
            // Their interpreter, at the level they asked for. Nobody
            // else's mix is touched by it.
            let gain = if l.interp.mute { 0.0 } else { l.interp.gain };
            let raw = interp.get(&l.language);
            let heard = raw.is_some() && gain > 0.0;
            if heard {
                let f = raw.expect("just checked");
                for (o, s) in theirs_interp.iter_mut().zip(f.iter()) {
                    *o = s * gain;
                }
            }
            // As loud as the interpreter is for them, and no louder: the
            // duck follows the fader, so a quiet interpreter leaves the
            // floor nearly where it was and one pulled all the way down
            // leaves it exactly there.
            let target = if heard { floor_under(gain) } else { 1.0 };
            let floor = st.floor.entry(l.id).or_insert(1.0);
            let duck = if *floor == 1.0 && target == 1.0 {
                None
            } else {
                for s in env.iter_mut() {
                    if *floor < target {
                        *floor = (*floor + DUCK_STEP).min(target);
                    } else if *floor > target {
                        *floor = (*floor - DUCK_STEP).max(target);
                    }
                    *s = *floor;
                }
                foreign.get(&l.language).map(|f| (&env, f))
            };
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
            let theirs = Theirs {
                ai: ai.get(&l.language),
                interp: heard.then_some(&theirs_interp),
                duck,
            };
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
/// What one listener hears over the room: the AI in their language, the
/// interpreter in their language, and how far the voices being
/// interpreted are taken down under it.
#[derive(Default, Clone, Copy)]
struct Theirs<'a> {
    ai: Option<&'a [f32; FRAME]>,
    interp: Option<&'a [f32; FRAME]>,
    duck: Option<(&'a [f32; FRAME], &'a [f32; FRAME])>,
}

fn mix_minus(
    total: &[f32; FRAME],
    own: Option<&[f32; FRAME]>,
    theirs: Theirs,
    out: &mut [f32; FRAME],
) {
    for i in 0..FRAME {
        let mut s = total[i] - own.map_or(0.0, |o| o[i]);
        if let Some((env, foreign)) = theirs.duck {
            s -= (1.0 - env[i]) * foreign[i];
        }
        s += theirs.ai.map_or(0.0, |a| a[i]) + theirs.interp.map_or(0.0, |v| v[i]);
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
        mix_minus(&total, Some(&a), Theirs::default(), &mut out);
        assert!(out.iter().all(|s| (s - 0.5).abs() < 1e-6));
        mix_minus(&total, Some(&b), Theirs::default(), &mut out);
        assert!(out.iter().all(|s| (s - 0.25).abs() < 1e-6));
        mix_minus(&total, None, Theirs::default(), &mut out);
        assert!(out.iter().all(|s| (s - 0.75).abs() < 1e-6));
    }

    fn ai_of(frame: &[f32; FRAME]) -> Theirs<'_> {
        Theirs {
            ai: Some(frame),
            ..Theirs::default()
        }
    }

    #[test]
    fn each_listener_hears_the_ai_in_one_language_and_no_other() {
        let english = [0.25f32; FRAME];
        let russian = [0.5f32; FRAME];
        let mics = [0.1f32; FRAME];
        let mut out = [0.0f32; FRAME];
        // The AI's languages are alternatives, never summed: whichever
        // one a listener is on is the only one they hear.
        mix_minus(&mics, None, ai_of(&english), &mut out);
        assert!(out.iter().all(|s| (s - 0.35).abs() < 1e-6));
        mix_minus(&mics, None, ai_of(&russian), &mut out);
        assert!(out.iter().all(|s| (s - 0.6).abs() < 1e-6));
        // And a mix still leaves out your own mic.
        mix_minus(&mics, Some(&mics), ai_of(&russian), &mut out);
        assert!(out.iter().all(|s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn the_duck_is_only_on_the_voices_being_interpreted() {
        // Somebody speaking a language this listener hasn't got, and
        // somebody speaking the one they have.
        let foreign = [0.4f32; FRAME];
        let same = [0.2f32; FRAME];
        let mut total = [0.0f32; FRAME];
        for i in 0..FRAME {
            total[i] = foreign[i] + same[i];
        }
        let voice = [0.1f32; FRAME];
        let env = [DUCK_GAIN; FRAME];
        let mut out = [0.0f32; FRAME];
        mix_minus(
            &total,
            None,
            Theirs {
                ai: None,
                interp: Some(&voice),
                duck: Some((&env, &foreign)),
            },
            &mut out,
        );
        // The interpreted voice down to a fifth, the one they can
        // follow left exactly where it was, and the interpreter over
        // the top of both.
        let want = 0.4 * DUCK_GAIN + 0.2 + 0.1;
        assert!(out.iter().all(|s| (s - want).abs() < 1e-6), "{}", out[0]);
    }

    #[test]
    fn an_interpretation_waits_while_the_ai_is_speaking_the_same_language() {
        let mixer = Mixer::new(BTreeMap::new(), "English");
        mixer.ai_push(AGENT_STRIP, "English", 1, 0, vec![0.5; 5 * FRAME]);
        mixer.interp_push("English", 7, 0, vec![0.3; 2 * FRAME]);
        for _ in 0..3 {
            mixer.tick();
        }
        // Both are in this listener's language, and two voices at once
        // is neither of them. The interpreter keeps its place.
        let p = mixer.interp_progress();
        assert_eq!(p["English"].current, Some((7, 0, 0, 2 * FRAME)));
        assert!(p["English"].finished.is_empty());

        // The AI finishes, and it is heard out from where it was.
        for _ in 0..4 {
            mixer.tick();
        }
        let p = mixer.interp_progress();
        assert!(
            !p["English"].finished.is_empty()
                || p["English"]
                    .current
                    .is_some_and(|(_, _, played, _)| played > 0),
            "the interpreter speaks once the AI has stopped: {:?}",
            p["English"]
        );
    }

    #[test]
    fn the_floor_drops_as_far_under_the_interpreter_as_the_fader_says() {
        // Off at the bottom: an interpreter nobody can hear is not
        // worth taking anything off the room for.
        assert_eq!(floor_under(0.0), 1.0);
        // The AI's own duck at unity, and the same again in dB at the
        // top of the fader -- a straight line in decibels.
        assert!((floor_under(1.0) - DUCK_GAIN).abs() < 1e-6);
        assert!((floor_under(MAX_GAIN) - DUCK_GAIN * DUCK_GAIN).abs() < 1e-6);
        // Monotone in between, and never past the fader's own limit.
        assert!(floor_under(0.5) > DUCK_GAIN && floor_under(0.5) < 1.0);
        assert_eq!(floor_under(10.0), floor_under(MAX_GAIN));
    }

    /// Two bots talking are two voices in the room, not two versions of
    /// one: a listener hears both, and each goes through a fader and a
    /// meter of its own. Nothing can make a second one yet; this is the
    /// mixer being ready for when something can.
    #[test]
    fn two_bots_are_two_voices_and_each_has_its_own_fader() {
        const OTHER: &str = "@agent:other";
        let mixer = Mixer::new(BTreeMap::new(), "English");
        let (tx, mut rx) = mpsc::channel(LISTENER_QUEUE);
        mixer.add_listener(1, "Ryan", "English", tx).unwrap();
        let mut d = opus::Decoder::new(RATE, opus::Channels::Mono).unwrap();
        // A few frames each time: Opus encodes with a little lookahead,
        // so the frame straight after a fader moves still carries the
        // tail of what was there before it.
        let hear = |mixer: &Mixer, rx: &mut mpsc::Receiver<Vec<u8>>, d: &mut opus::Decoder| {
            let mut last = 0.0;
            for _ in 0..4 {
                mixer.tick();
                last = peak(&heard_by(rx, d));
            }
            last
        };

        // Both say the same thing at once, which is the loudest the two
        // of them can be together.
        mixer.ai_push(AGENT_STRIP, "English", 1, 0, tone(40, 0.25, &mut 0));
        mixer.ai_push(OTHER, "English", 1, 0, tone(40, 0.25, &mut 0));
        let both = hear(&mixer, &mut rx, &mut d);
        assert!(both > 0.4, "two voices, not one: {both}");

        // One of them is turned off, and the other carries on.
        mixer.set_strip(OTHER, None, Some(true));
        let alone = hear(&mixer, &mut rx, &mut d);
        assert!(
            alone > 0.15 && alone < both * 0.75,
            "{alone} against {both}: one fader is one bot"
        );

        // And each said so on a meter of its own.
        let levels = mixer.take_levels();
        assert!(levels.contains_key(AGENT_STRIP), "{levels:?}");
        assert!(levels.contains_key(OTHER), "{levels:?}");

        // Stopping one leaves the other where it was.
        mixer.set_strip(OTHER, None, Some(false));
        let over = mixer.ai_clear(OTHER);
        assert!(over.contains_key("English"));
        assert!(
            !mixer.ai_progress(AGENT_STRIP)["English"].idle,
            "the other one is still talking"
        );
    }

    #[test]
    fn an_interpreter_turned_down_is_turned_down_for_one_listener() {
        // Two people on the same language, one of whom wants no
        // interpreter. It is their own mix they are changing, not the
        // room's: there is no shared fader left to reach for.
        let mixer = Mixer::new(BTreeMap::new(), "English");
        let (tx, mut to_ryan) = mpsc::channel(LISTENER_QUEUE);
        mixer.add_listener(1, "Ryan", "English", tx).unwrap();
        let (tx, mut to_mike) = mpsc::channel(LISTENER_QUEUE);
        mixer.add_listener(2, "Mike", "English", tx).unwrap();
        let (tx, _to_anya) = mpsc::channel(LISTENER_QUEUE);
        mixer.add_listener(3, "Anya", "Russian", tx).unwrap();
        mixer.set_listener_interp(1, Some(0.0), None);
        assert!(
            mixer.interp_wanted("English"),
            "Mike still wants one, so it is still worth saying"
        );

        let mut dr = opus::Decoder::new(RATE, opus::Channels::Mono).unwrap();
        let mut dm = opus::Decoder::new(RATE, opus::Channels::Mono).unwrap();
        let mut at = 0;
        mixer.push_mic("Anya", &tone(3, 0.4, &mut at));
        let mut turn = |mixer: &Mixer, at: &mut usize| {
            mixer.push_mic("Anya", &tone(1, 0.4, at));
            mixer.tick();
            (
                peak(&heard_by(&mut to_ryan, &mut dr)),
                peak(&heard_by(&mut to_mike, &mut dm)),
            )
        };
        let mut before = (0.0, 0.0);
        for _ in 0..4 {
            before = turn(&mixer, &mut at);
        }

        mixer.interp_push("English", 7, 0, tone(8, 0.5, &mut 0));
        let mut after = (0.0, 0.0);
        for _ in 0..4 {
            after = turn(&mixer, &mut at);
        }
        // Ryan: nothing over Anya and nothing taken off her, which is
        // what being turned off means.
        assert!(
            (after.0 - before.0).abs() < 0.03,
            "Ryan: {} against {}, with his interpreter off",
            after.0,
            before.0
        );
        // Mike, beside him and on the same language: the interpreter is
        // in his mix, and Anya has dropped away under it.
        assert!(
            after.1 > before.1 + 0.05 && after.1 > after.0 + 0.05,
            "Mike: {} against {}, and Ryan's {}",
            after.1,
            before.1,
            after.0
        );

        // The queue drains all the same, so the words on screen keep
        // pace with a voice somebody may be about to bring back up.
        let p = mixer.interp_progress();
        assert!(
            p["English"]
                .current
                .is_some_and(|(_, _, played, _)| played > 0),
            "{:?}",
            p["English"]
        );
    }

    /// A steady tone, which is what survives Opus well enough to compare
    /// one frame with the next. Flat DC does not: the codec high-passes
    /// it away and leaves only the edges, which ring.
    fn tone(frames: usize, amp: f32, at: &mut usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames * FRAME);
        for _ in 0..frames * FRAME {
            let t = *at as f32 / RATE as f32;
            out.push(amp * (std::f32::consts::TAU * 300.0 * t).sin());
            *at += 1;
        }
        out
    }

    #[test]
    fn an_interpretation_is_heard_by_the_people_it_is_for_and_nobody_else() {
        let mixer = Mixer::new(BTreeMap::new(), "English");
        let (tx, mut to_ryan) = mpsc::channel(LISTENER_QUEUE);
        mixer.add_listener(1, "Ryan", "English", tx).unwrap();
        let (tx, mut to_anya) = mpsc::channel(LISTENER_QUEUE);
        mixer.add_listener(2, "Anya", "Russian", tx).unwrap();
        let mut d1 = opus::Decoder::new(RATE, opus::Channels::Mono).unwrap();
        let mut d2 = opus::Decoder::new(RATE, opus::Channels::Mono).unwrap();
        // Anya is talking, in Russian, and keeps talking throughout.
        let mut at = 0;
        mixer.push_mic("Anya", &tone(3, 0.4, &mut at));
        let mut turn = |mixer: &Mixer, at: &mut usize| {
            mixer.push_mic("Anya", &tone(1, 0.4, at));
            mixer.tick();
            (
                peak(&heard_by(&mut to_ryan, &mut d1)),
                peak(&heard_by(&mut to_anya, &mut d2)),
            )
        };
        let mut before = 0.0;
        for _ in 0..4 {
            before = turn(&mixer, &mut at).0;
        }
        assert!(before > 0.3, "Ryan hears her as she is: {before}");

        // What she said, in English, for the people who don't have her
        // language. Her voice goes down under it; Anya's own mix has no
        // interpreter in it and is left alone.
        mixer.interp_push("English", 7, 0, tone(8, 0.05, &mut 0));
        let (mut ryan, mut anya) = (0.0, 0.0);
        for _ in 0..4 {
            (ryan, anya) = turn(&mixer, &mut at);
        }
        assert!(
            ryan < before / 2.0,
            "the voice being interpreted drops: {ryan} against {before}"
        );
        assert!(
            anya < 0.02,
            "and Anya, who has no interpreter, hears what she always did: \
             nothing but the room, which is only herself"
        );
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
        mixer.ai_push(AGENT_STRIP, "English", 1, 0, vec![0.5; 20 * FRAME]);
        mixer.tick();
        assert!(
            peak(&heard_by(&mut rx, &mut decoder)) < 0.02,
            "a listener who asked for Russian must not be handed English \
             while the translation is on its way"
        );

        // Their own words arrive, and now they hear them.
        mixer.ai_push(AGENT_STRIP, "Russian", 1, 0, vec![0.5; 20 * FRAME]);
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
        mixer.ai_push(AGENT_STRIP, "English", 7, 0, vec![0.5; FRAME / 2]);
        mixer.ai_push(AGENT_STRIP, "Russian", 7, 0, vec![0.5; 2 * FRAME]);
        mixer.tick();

        let p = mixer.ai_progress(AGENT_STRIP);
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
        let p = mixer.ai_progress(AGENT_STRIP);
        assert!(p["English"].finished.is_empty());
        assert_eq!(p["Russian"].finished, vec![(7, 0, 2 * FRAME)]);
        assert!(p["Russian"].idle, "now both are done");
    }

    #[test]
    fn stopping_the_ai_stops_every_language_at_once() {
        let mixer = Mixer::new(BTreeMap::new(), "English");
        mixer.ai_push(AGENT_STRIP, "English", 3, 0, vec![0.5; 10 * FRAME]);
        mixer.ai_push(AGENT_STRIP, "Russian", 3, 0, vec![0.5; 10 * FRAME]);
        mixer.tick();

        let p = mixer.ai_clear(AGENT_STRIP);
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
        for (_, after) in mixer.ai_progress(AGENT_STRIP) {
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
        let mut ai = Track::default();
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
        let mut ai = Track::default();
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
