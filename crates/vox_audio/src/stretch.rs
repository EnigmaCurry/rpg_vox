//! Time stretching for faster or slower playback without changing the
//! pitch: WSOLA (waveform-similarity overlap-add). Windows of the input
//! are overlap-added at a fixed output hop while the read position
//! advances by `hop × speed`; each window is nudged (within a few ms) to
//! where it best continues the previous one, which keeps voices clean.
//! At 1× it passes the audio through untouched.

/// A 48 kHz-oriented stretcher; the sizes are in samples.
pub struct Stretch {
    speed: f64,
    win: usize,
    hop: usize,
    tol: usize,
    window: Vec<f32>,
    /// Input not yet consumed; `input[0]` is sample `base` of the stream.
    input: Vec<f32>,
    /// Where the next window would start with no adjustment.
    pos: f64,
    /// Where the input naturally continued after the previous window
    /// (its start plus one hop): what the next window should match.
    follow: Option<usize>,
    /// Second half of the previous window, waiting for its overlap.
    tail: Vec<f32>,
}

impl Stretch {
    /// `win_ms` windows at `rate`, overlapped by half; `tol_ms` search.
    pub fn new(rate: u32) -> Self {
        let ms = |m: u32| (rate as usize * m as usize / 1000).max(2);
        let win = ms(40) & !1;
        let hop = win / 2;
        // Periodic Hann: two half-overlapped windows sum to 1.
        let window = (0..win)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / win as f32).cos())
            .collect();
        Self {
            speed: 1.0,
            win,
            hop,
            tol: ms(10),
            window,
            input: Vec::new(),
            pos: 0.0,
            follow: None,
            tail: vec![0.0; hop],
        }
    }

    pub fn speed(&self) -> f64 {
        self.speed
    }

    /// Change speed; also drops anything buffered (call before a seek).
    pub fn set_speed(&mut self, speed: f64) {
        self.speed = speed.clamp(0.25, 4.0);
        self.reset();
    }

    pub fn reset(&mut self) {
        self.input.clear();
        self.pos = 0.0;
        self.follow = None;
        self.tail.iter_mut().for_each(|x| *x = 0.0);
    }

    /// Feed `input`, appending what can be produced to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if (self.speed - 1.0).abs() < 1e-9 {
            out.extend_from_slice(input);
            return;
        }
        self.input.extend_from_slice(input);
        loop {
            let nominal = self.pos.round() as usize;
            let need = nominal + self.tol + self.win;
            let natural_end = self.follow.map(|f| f + self.hop).unwrap_or(0);
            if self.input.len() < need.max(natural_end) {
                break;
            }
            let start = match self.follow {
                None => nominal,
                Some(f) => self.best_start(nominal, f),
            };
            let frame = &self.input[start..start + self.win];
            let (head, rest) = frame.split_at(self.hop);
            let (w_head, w_rest) = self.window.split_at(self.hop);
            out.extend(
                self.tail
                    .iter()
                    .zip(head.iter().zip(w_head))
                    .map(|(t, (x, w))| t + x * w),
            );
            for (t, (x, w)) in self.tail.iter_mut().zip(rest.iter().zip(w_rest)) {
                *t = x * w;
            }
            self.follow = Some(start + self.hop);
            self.pos += self.hop as f64 * self.speed;
            self.trim();
        }
    }

    /// The window start near `nominal` whose first half best matches
    /// the input that naturally followed the previous window (at `follow`).
    fn best_start(&self, nominal: usize, follow: usize) -> usize {
        let lo = nominal.saturating_sub(self.tol);
        let hi = nominal + self.tol;
        let target = &self.input[follow..follow + self.hop];
        let mut best = (f32::MIN, nominal);
        // Every other lag and sample: plenty for speech, half the cost.
        for s in (lo..=hi).step_by(2) {
            let cand = &self.input[s..s + self.hop];
            let score: f32 = cand.iter().zip(target).step_by(2).map(|(a, b)| a * b).sum();
            if score > best.0 {
                best = (score, s);
            }
        }
        best.1
    }

    /// Drop input that no future window can reach.
    fn trim(&mut self) {
        let keep_from = (self.pos as usize)
            .saturating_sub(self.tol)
            .min(self.follow.unwrap_or(usize::MAX));
        if keep_from > 0 {
            self.input.drain(..keep_from.min(self.input.len()));
            self.pos -= keep_from as f64;
            self.follow = self.follow.map(|f| f - keep_from);
        }
    }

    /// End of input: emit the last half window.
    pub fn flush(&mut self, out: &mut Vec<f32>) {
        if (self.speed - 1.0).abs() >= 1e-9 && self.follow.is_some() {
            out.extend_from_slice(&self.tail);
        }
        self.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(n: usize, hz: f32) -> Vec<f32> {
        (0..n)
            .map(|i| (std::f32::consts::TAU * hz * i as f32 / 48_000.0).sin() * 0.5)
            .collect()
    }

    /// Dominant period of `x` in samples, by autocorrelation.
    fn period(x: &[f32]) -> usize {
        (20..400)
            .max_by(|&a, &b| {
                let c = |l: usize| x.iter().zip(&x[l..]).map(|(p, q)| p * q).sum::<f32>();
                c(a).total_cmp(&c(b))
            })
            .unwrap()
    }

    #[test]
    fn changes_length_not_pitch() {
        let input = tone(48_000 * 2, 200.0);
        for speed in [0.5, 1.5, 2.0] {
            let mut s = Stretch::new(48_000);
            s.set_speed(speed);
            let mut out = Vec::new();
            for c in input.chunks(960) {
                s.process(c, &mut out);
            }
            s.flush(&mut out);
            let want = (input.len() as f64 / speed) as i64;
            assert!(
                (out.len() as i64 - want).abs() < 4_000,
                "{speed}: {} vs {want}",
                out.len()
            );
            // 200 Hz at 48 kHz: a 240-sample period, before and after.
            let mid = &out[out.len() / 4..out.len() / 4 + 4_800];
            assert!(
                (period(mid) as i64 - 240).abs() <= 2,
                "{speed}: {}",
                period(mid)
            );
        }
    }

    #[test]
    fn one_x_is_untouched() {
        let input = tone(5_000, 300.0);
        let mut s = Stretch::new(48_000);
        let mut out = Vec::new();
        s.process(&input, &mut out);
        assert_eq!(out, input);
    }
}
