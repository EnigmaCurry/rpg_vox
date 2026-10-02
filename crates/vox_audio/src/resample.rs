//! Streaming linear-interpolation resampler. Plenty for speech going to
//! Opus or a speaker; not meant for music mastering.

pub struct Linear {
    /// Input samples per output sample.
    step: f64,
    /// Position of the next output sample, in input samples relative to
    /// the start of the next chunk (-1.0 is the last sample of the
    /// previous chunk).
    t: f64,
    prev: f32,
    identity: bool,
}

impl Linear {
    pub fn new(from_rate: u32, to_rate: u32) -> Self {
        Self {
            step: from_rate as f64 / to_rate.max(1) as f64,
            t: 0.0,
            prev: 0.0,
            identity: from_rate == to_rate,
        }
    }

    /// Resample `input`, appending to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.identity {
            out.extend_from_slice(input);
            return;
        }
        let n = input.len();
        if n == 0 {
            return;
        }
        let at = |i: isize| if i < 0 { self.prev } else { input[i as usize] };
        // Interpolating needs the sample after `t`, so stop short of the
        // chunk's last sample; the rest happens with the next chunk.
        while self.t < (n - 1) as f64 {
            let i = self.t.floor();
            let frac = (self.t - i) as f32;
            let i = i as isize;
            let (a, b) = (at(i), at(i + 1));
            out.push(a + (b - a) * frac);
            self.t += self.step;
        }
        self.t -= n as f64;
        self.prev = input[n - 1];
    }

    /// Drop history, e.g. after a seek.
    pub fn reset(&mut self) {
        self.t = 0.0;
        self.prev = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_length_across_chunks() {
        let mut r = Linear::new(16_000, 48_000);
        let mut out = Vec::new();
        for _ in 0..100 {
            r.process(&[0.5; 160], &mut out);
        }
        assert!((out.len() as i64 - 48_000).abs() <= 3, "{}", out.len());
        let mut r = Linear::new(44_100, 48_000);
        let mut out = Vec::new();
        for _ in 0..100 {
            r.process(&[0.0; 441], &mut out);
        }
        assert!((out.len() as i64 - 48_000).abs() <= 3, "{}", out.len());
    }
}
