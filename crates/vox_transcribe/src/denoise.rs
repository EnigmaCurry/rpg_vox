//! Optional speech enhancement before the VAD and every pass:
//! DeepFilterNet 3 (built in with the `deepfilter` feature) takes out
//! background music and noise and keeps the voice.
//!
//! The model runs at 48 kHz in 10 ms frames, so audio is resampled up
//! and back down around it. Output stays sample-for-sample in step with
//! the input (a fixed delay of about one frame), so the engine's audio
//! clock and timestamps are unchanged.

// Without the feature only the stub `new` is reachable.
#![cfg_attr(not(feature = "deepfilter"), allow(dead_code))]

use std::collections::VecDeque;

use anyhow::Result;

/// The model's sample rate.
const DF_RATE: u32 = 48_000;

pub struct Denoiser {
    #[cfg(feature = "deepfilter")]
    df: df::tract::DfTract,
    up: Linear,
    down: Linear,
    /// 48 kHz input waiting for a whole frame.
    frame_in: Vec<f32>,
    /// Enhanced audio at the engine rate, primed with the delay.
    ready: VecDeque<f32>,
    scratch: Vec<f32>,
}

impl Denoiser {
    /// `atten_lim_db` caps how far noise is pushed down (e.g. 20 dB
    /// leaves some background in, which can sound and decode more
    /// naturally); `None` removes all it can.
    #[cfg(feature = "deepfilter")]
    pub fn new(rate: u32, atten_lim_db: Option<f32>) -> Result<Self> {
        use df::tract::{DfParams, DfTract, RuntimeParams};
        let mut rp = RuntimeParams::default_with_ch(1);
        if let Some(db) = atten_lim_db {
            rp = rp.with_atten_lim(db);
        }
        let df = DfTract::new(DfParams::default(), &rp)?;
        anyhow::ensure!(df.sr == DF_RATE as usize, "unexpected model rate {}", df.sr);
        // One frame back at the engine rate, plus a little for the
        // resamplers' carry-over.
        let delay = (df.hop_size as u64 * rate as u64).div_ceil(DF_RATE as u64) as usize + 4;
        Ok(Self {
            frame_in: Vec::with_capacity(df.hop_size * 2),
            df,
            up: Linear::new(rate, DF_RATE),
            down: Linear::new(DF_RATE, rate),
            ready: std::iter::repeat_n(0.0, delay).collect(),
            scratch: Vec::new(),
        })
    }

    #[cfg(not(feature = "deepfilter"))]
    pub fn new(_rate: u32, _atten_lim_db: Option<f32>) -> Result<Self> {
        anyhow::bail!("this build has no noise filter (the `deepfilter` feature)")
    }

    /// Enhance `mono`, returning exactly as many samples.
    pub fn process(&mut self, mono: &[f32]) -> Vec<f32> {
        self.up.process(mono, &mut self.frame_in);
        #[cfg(feature = "deepfilter")]
        {
            use ndarray::{ArrayView2, ArrayViewMut2};
            let hop = self.df.hop_size;
            let mut enhanced = vec![0.0f32; hop];
            let mut done = 0;
            while self.frame_in.len() - done >= hop {
                let noisy = ArrayView2::from_shape((1, hop), &self.frame_in[done..done + hop])
                    .expect("frame shape");
                let enh = ArrayViewMut2::from_shape((1, hop), &mut enhanced).expect("frame shape");
                if let Err(e) = self.df.process(noisy, enh) {
                    tracing::warn!(error = %e, "noise filter failed on a frame; passing it through");
                    enhanced.copy_from_slice(&self.frame_in[done..done + hop]);
                }
                self.scratch.clear();
                self.down.process(&enhanced, &mut self.scratch);
                self.ready.extend(self.scratch.iter());
                done += hop;
            }
            self.frame_in.drain(..done);
        }
        let n = mono.len();
        let have = self.ready.len().min(n);
        let mut out: Vec<f32> = self.ready.drain(..have).collect();
        // Only short if the delay was misjudged; keep the clock anyway.
        out.resize(n, 0.0);
        out
    }
}

/// Streaming linear-interpolation resampler (as in `vox_audio`, which
/// this crate doesn't depend on).
struct Linear {
    step: f64,
    t: f64,
    prev: f32,
    identity: bool,
}

impl Linear {
    fn new(from: u32, to: u32) -> Self {
        Self {
            step: from as f64 / to.max(1) as f64,
            t: 0.0,
            prev: 0.0,
            identity: from == to,
        }
    }

    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.identity {
            out.extend_from_slice(input);
            return;
        }
        let n = input.len();
        if n == 0 {
            return;
        }
        let at = |i: isize| if i < 0 { self.prev } else { input[i as usize] };
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
}

#[cfg(all(test, feature = "deepfilter"))]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_sample_count() {
        let mut d = Denoiser::new(16_000, None).unwrap();
        let mut total = 0;
        for n in [1, 160, 333, 1600, 7] {
            let input: Vec<f32> = (0..n).map(|i| (i as f32 * 0.05).sin() * 0.1).collect();
            total += d.process(&input).len();
        }
        assert_eq!(total, 1 + 160 + 333 + 1600 + 7);
    }
}
