//! Procedural ambient "raindrop" filler generator.
//!
//! Produces a soft, sparse bed of pinged sine tones and occasional lower
//! plops, layered over a very quiet bandpassed-noise fizz. The intent is
//! filler that's easy to sit under for the multi-minute LLM wait — closer
//! to distant rain than to a teletype rattle.
//!
//! Each event is a pure sine tone with a rounded envelope (fast soft
//! attack + exponential decay), scheduled at an absolute sample position
//! so drops that straddle buffer boundaries stitch together seamlessly.
//! The fizz bed is a per-sample noise source that's continuous across
//! render calls — no click at the seam.
//!
//! Stateful streaming: [`ClickGenerator::render_next`] fills a stereo
//! buffer of a caller-chosen frame count.

use std::collections::VecDeque;

/// Which filler bed to render. Selected per-agent (stored on the agent
/// row as an `interstitial` string) and threaded through the mixer as
/// a u8 so the RT-side task can read it without a mutex. Extend by
/// adding a variant, a `str_id` mapping, and a branch in
/// [`ClickGenerator::event_stereo_at`] tuned for the new voicing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickPreset {
    /// Rain-drop bed: light high-sine pings, sparse lower plops, and a
    /// very quiet filtered-noise fizz. The only preset shipped today;
    /// the default when a user turns interstitial filler on. The str-id
    /// stays `"vintage"` for schema compat with agent rows written
    /// under the earlier teletype voicing — the internal timbre has
    /// changed but the persisted enum tag hasn't.
    Vintage,
}

impl ClickPreset {
    /// Stable string id used in HTTP bodies + on the agent DB row.
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vintage => "vintage",
        }
    }

    /// Inverse of [`Self::as_str`]. Returns `None` for unknown ids so
    /// the HTTP layer can respond with a clear 400 instead of silently
    /// falling back to a default preset.
    pub fn from_str_id(s: &str) -> Option<Self> {
        match s {
            "vintage" => Some(Self::Vintage),
            _ => None,
        }
    }

    /// Non-zero u8 encoding for the mixer atomic. 0 is reserved for
    /// "off" so callers can use `!= 0` as a cheap "clicks active" check
    /// without a load-and-decode step.
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Vintage => 1,
        }
    }

    /// Decode a u8 read from the mixer atomic. 0 → None (off); any
    /// unknown non-zero value also returns None so a corrupt read is
    /// treated as off rather than crashing.
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Vintage),
            _ => None,
        }
    }
}

/// A single droplet event scheduled at an absolute sample position.
#[derive(Debug, Clone, Copy)]
struct Event {
    /// Absolute sample index at which this event's envelope begins.
    start: u64,
    kind: EventKind,
    /// Peak amplitude before the envelope shapes it. Small per-event
    /// scatter keeps successive drops from sounding identical.
    amp: f32,
    /// Equal-power pan in `[-1, +1]`. Pings scatter wider than plops so
    /// the bed reads as "raindrops in a space" instead of a single
    /// point source.
    pan: f32,
    /// Sine frequency in Hz. Randomised inside the kind's range so no
    /// two drops share exactly the same note.
    freq: f32,
}

#[derive(Debug, Clone, Copy)]
enum EventKind {
    /// Bright, brief drop — a small round sine in the 2–4 kHz range.
    /// The common voice of the bed.
    Ping,
    /// Lower, rounder drop — a soft plop in the 150–300 Hz range.
    /// Rare accent so the bed has vertical variety without turning
    /// into a bassline.
    Plob,
}

pub struct ClickGenerator {
    /// Which flavour of bed to render. Kept as a field so the runner
    /// can tell whether the currently-generating instance matches the
    /// mixer's active preset — mismatch means "user switched presets
    /// mid-wait" and the runner should drop this generator and start
    /// a fresh one on the new preset.
    preset: ClickPreset,
    sample_rate: u32,
    /// Absolute sample cursor. Advances by `frames` on each render call.
    cursor: u64,
    /// Absolute sample position at which we should next schedule a ping.
    /// The scheduler is decoupled from the buffer boundary — drops can
    /// straddle multiple render calls cleanly because they're keyed to
    /// this global timeline.
    next_ping_at: u64,
    /// Absolute sample position at which we should next schedule a plob.
    /// Independent from `next_ping_at` so the two patterns interleave.
    next_plob_at: u64,
    /// Live events. Each one bleeds a decaying tail across many render
    /// calls; we keep them here until their envelope has fallen below
    /// audibility, at which point they're dropped.
    events: VecDeque<Event>,
    /// LCG state for jitter / amplitude / frequency scatter and for the
    /// fizz noise source. Enough randomness for an aesthetic bed
    /// without pulling in a `rand` dependency.
    rng: u64,
    /// One-pole low-pass state for the fizz bed. Fizz is a shared mono
    /// noise source that carries across every render call, so the
    /// filter tail has to persist too — otherwise you'd hear a subtle
    /// discontinuity at each buffer boundary.
    fizz_lp: f32,
    /// One-pole high-pass state for the fizz bed. Same reasoning as
    /// [`Self::fizz_lp`].
    fizz_hp: f32,
    /// Previous filtered LP sample fed into the fizz HP — the second
    /// half of the one-pole HP topology.
    fizz_hp_prev_in: f32,
}

impl ClickGenerator {
    pub fn new(preset: ClickPreset, sample_rate: u32) -> Self {
        let mut g = Self {
            preset,
            sample_rate,
            cursor: 0,
            next_ping_at: 0,
            next_plob_at: 0,
            events: VecDeque::new(),
            // Seed with a golden-ratio-ish constant so two starts don't
            // produce identical patterns. Not for security — just variety.
            rng: 0x9E37_79B9_7F4A_7C15,
            fizz_lp: 0.0,
            fizz_hp: 0.0,
            fizz_hp_prev_in: 0.0,
        };
        // Delay the first plob 3–7 s out so early wait time is just
        // pings — the plob then reads as an accent rather than an intro.
        g.next_plob_at = ((3.0 + g.rand01() * 4.0) * sample_rate as f32) as u64;
        g
    }

    /// Which preset this generator is voicing. The runner compares this
    /// against the mixer's live selection between bursts and rebuilds
    /// on mismatch.
    #[inline]
    pub fn preset(&self) -> ClickPreset {
        self.preset
    }

    /// Advance a 64-bit LCG. Returns [0, 1). MMIX constants from Knuth.
    fn rand01(&mut self) -> f32 {
        self.rng = self
            .rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        // Take the top 24 bits — LCG lower bits have short cycles.
        let bits = (self.rng >> 40) as u32;
        (bits as f32) / ((1u32 << 24) as f32)
    }

    /// Symmetric [-1, +1] variant of [`Self::rand01`].
    fn rand_pm(&mut self) -> f32 {
        self.rand01() * 2.0 - 1.0
    }

    /// Schedule any events that should land between the cursor and
    /// `end_sample + 1s` — the extra second of lead lets us push events
    /// whose start is past the buffer end so their envelopes render in
    /// the NEXT call from position 0.
    fn schedule(&mut self, end_sample: u64) {
        let sr = self.sample_rate as f32;

        // Pings: ~1.5–2.5 Hz base rate with heavy jitter. Sparser than
        // the old teletype ticks so the bed doesn't feel busy over long
        // waits.
        while self.next_ping_at < end_sample + (sr as u64) {
            let jitter_samples = ((self.rand_pm() * 0.150) * sr) as i64;
            let start_signed = self.next_ping_at as i64 + jitter_samples;
            let start = start_signed.max(self.cursor as i64) as u64;
            let amp = 0.10 * (0.7 + 0.6 * self.rand01());
            // Wide pan on the pings — feels like raindrops falling across
            // the field, not a single spot. Constant-power L/R happens
            // inside event_stereo_at.
            let pan = self.rand_pm() * 0.6;
            // 2.2–3.8 kHz — sits above speech comfortably, sine so it
            // doesn't compete with content.
            let freq = 2200.0 + self.rand01() * 1600.0;
            self.events.push_back(Event {
                start,
                kind: EventKind::Ping,
                amp,
                pan,
                freq,
            });
            // ~500–750 ms base interval → 1.3–2 Hz once you add jitter.
            let interval_ms = 500.0 + self.rand01() * 250.0;
            self.next_ping_at = start + ((interval_ms / 1000.0) * sr) as u64;
        }

        // Plobs: 4–8 s cadence. Rare, so they punctuate rather than pace.
        while self.next_plob_at < end_sample + (sr as u64) {
            let jitter_samples = ((self.rand_pm() * 0.200) * sr) as i64;
            let start_signed = self.next_plob_at as i64 + jitter_samples;
            let start = start_signed.max(self.cursor as i64) as u64;
            let amp = 0.14 * (0.8 + 0.4 * self.rand01());
            // Plobs sit closer to centre — a lopsided low-frequency
            // thump would sound like the drop hit the wall.
            let pan = self.rand_pm() * 0.25;
            let freq = 160.0 + self.rand01() * 140.0; // 160–300 Hz
            self.events.push_back(Event {
                start,
                kind: EventKind::Plob,
                amp,
                pan,
                freq,
            });
            let interval_s = 4.0 + self.rand01() * 4.0;
            self.next_plob_at = start + (interval_s * sr) as u64;
        }
    }

    /// Contribution of one event at absolute sample `n`, as a stereo
    /// pair. Envelope is a fast soft attack times exponential decay —
    /// rounded enough that the transient doesn't click, short enough
    /// that the drop still feels like a drop and not a note. Returns
    /// zeros if the event hasn't started yet or has decayed below
    /// audibility.
    #[inline]
    fn event_stereo_at(&self, ev: &Event, n: u64) -> [f32; 2] {
        if n < ev.start {
            return [0.0, 0.0];
        }
        let sr = self.sample_rate as f32;
        let t = (n - ev.start) as f32 / sr;
        // Attack shapes the leading edge — sharp enough to still read
        // as a drop, soft enough that the transient isn't a click.
        // Decay controls the tail. Plobs get a slower attack + longer
        // decay so they land as "plop" rather than "thump".
        let (attack_tau, decay_tau) = match ev.kind {
            EventKind::Ping => (0.002, 0.055),
            EventKind::Plob => (0.008, 0.180),
        };
        let attack = 1.0 - (-t / attack_tau).exp();
        let decay = (-t / decay_tau).exp();
        let env = attack * decay;
        if env < 1.0e-4 {
            return [0.0, 0.0];
        }
        let sample = env * (2.0 * std::f32::consts::PI * ev.freq * t).sin();
        let sig = ev.amp * sample;
        // Equal-power pan: p=0 gives L=R=sig·√½.
        let p = ev.pan.clamp(-1.0, 1.0);
        let angle = (p + 1.0) * std::f32::consts::FRAC_PI_4;
        [sig * angle.cos(), sig * angle.sin()]
    }

    /// Render `frames` stereo samples into `out`. `out` must be sized
    /// to `frames`. Any events whose tail has fully decayed by the end
    /// of the buffer are dropped so the event queue stays bounded even
    /// over long runs.
    pub fn render_next(&mut self, out: &mut [[f32; 2]]) {
        let frames = out.len();
        for slot in out.iter_mut() {
            *slot = [0.0, 0.0];
        }
        let start = self.cursor;
        let end = start + frames as u64;
        self.schedule(end);

        // Fizz bed coefficients — one-pole LP+HP roughly 1.4–3.2 kHz
        // bandpass at 48 kHz. Amplitude kept very low so it reads as
        // "distant patter" rather than tape hiss. Filter state carries
        // across render calls so the bed is genuinely continuous with
        // no seam at buffer boundaries.
        let fizz_lp_a = 0.35;
        let fizz_hp_a = 0.90;
        let fizz_amp = 0.014;

        // Snapshot queue length so mid-loop pushes (there aren't any
        // now, but future-proofing) don't affect the iteration bound.
        let n_events = self.events.len();
        for i in 0..frames {
            let n = start + i as u64;

            // Sum the sine drops. Each event's stereo pair is already
            // panned — no post-hoc filter or pan pass needed.
            let mut l = 0.0f32;
            let mut r = 0.0f32;
            for e in 0..n_events {
                let ev = self.events[e];
                let [el, er] = self.event_stereo_at(&ev, n);
                l += el;
                r += er;
            }

            // Fizz bed: shared mono noise, bandpassed, mixed in equally
            // to both channels. Draws after the event loop so the RNG
            // pattern isn't correlated with drop timing.
            let noise = self.rand_pm();
            self.fizz_lp = self.fizz_lp + fizz_lp_a * (noise - self.fizz_lp);
            let lp_out = self.fizz_lp;
            let hp_out = fizz_hp_a * (self.fizz_hp + lp_out - self.fizz_hp_prev_in);
            self.fizz_hp = hp_out;
            self.fizz_hp_prev_in = lp_out;
            let fizz = fizz_amp * hp_out;

            out[i][0] = (l + fizz).clamp(-1.0, 1.0);
            out[i][1] = (r + fizz).clamp(-1.0, 1.0);
        }

        // Drop events whose envelope has fully decayed by the end of
        // the buffer. Plobs decay slowest, so a 400 ms tail covers both
        // kinds without cutting a plob mid-tail.
        let tail = (self.sample_rate as u64) * 400 / 1000;
        self.events
            .retain(|ev| ev.start.saturating_add(tail) >= end);

        self.cursor = end;
    }
}
