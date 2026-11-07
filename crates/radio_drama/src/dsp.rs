//! The small amount of signal processing a speech-only production needs:
//! trimming the silence the synthesizer pads with, ramping the edges so
//! concatenation does not click, level, and a band-pass with a little
//! saturation for the lines marked `(over vox)`.
//!
//! All of it runs at mix time on the cached synth output rather than
//! before caching, so changing a gain or a filter re-mixes in seconds
//! instead of re-synthesizing the play.

use crate::cast::DirectionEffect;

pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

pub fn apply_gain(samples: &mut [f32], db: f32) {
    if db == 0.0 {
        return;
    }
    let g = db_to_gain(db);
    for s in samples.iter_mut() {
        *s *= g;
    }
}

/// Cut the leading and trailing silence, keeping `keep` samples of it so
/// a cut never lands on the attack of the first word or the tail of the
/// last.
///
/// The threshold is on a short sliding window rather than a single
/// sample, because a single sample crossing a threshold is as likely to
/// be noise in the reference recording as the start of speech.
pub fn trim_silence(samples: &[f32], threshold_db: f32, keep: usize, rate: u32) -> (usize, usize) {
    let window = ((rate as usize) / 200).max(1); // 5 ms
    let threshold = db_to_gain(threshold_db);
    let loud = |at: usize| -> bool {
        let end = (at + window).min(samples.len());
        samples[at..end].iter().any(|s| s.abs() > threshold)
    };

    let mut start = 0;
    while start + window <= samples.len() && !loud(start) {
        start += window;
    }
    if start + window > samples.len() {
        // Nothing in it crosses the threshold. Rather than return an
        // empty span — which would silently drop a line — hand back the
        // whole thing and let the level speak for itself.
        return (0, samples.len());
    }

    let mut end = samples.len();
    while end >= window && !loud(end - window) {
        end -= window;
    }

    (start.saturating_sub(keep), (end + keep).min(samples.len()))
}

/// Linear ramp in and out, in place.
pub fn fade_edges(samples: &mut [f32], fade: usize) {
    let fade = fade.min(samples.len() / 2);
    if fade == 0 {
        return;
    }
    let n = samples.len();
    for i in 0..fade {
        let g = i as f32 / fade as f32;
        samples[i] *= g;
        samples[n - 1 - i] *= g;
    }
}

/// One biquad section, applied in place.
///
/// Butterworth Q (1/sqrt(2)) for both kinds, which is the flattest thing
/// a single section can be and is what "band-limited like a vox channel"
/// means in practice. Coefficients are the Audio EQ Cookbook's.
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Biquad {
    fn lowpass(cutoff: f32, rate: u32) -> Self {
        let (cos_w0, alpha, norm) = Self::shared(cutoff, rate);
        let b0 = (1.0 - cos_w0) / 2.0;
        Self {
            b0: b0 / norm,
            b1: (1.0 - cos_w0) / norm,
            b2: b0 / norm,
            a1: (-2.0 * cos_w0) / norm,
            a2: (1.0 - alpha) / norm,
        }
    }

    fn highpass(cutoff: f32, rate: u32) -> Self {
        let (cos_w0, alpha, norm) = Self::shared(cutoff, rate);
        let b0 = (1.0 + cos_w0) / 2.0;
        Self {
            b0: b0 / norm,
            b1: (-(1.0 + cos_w0)) / norm,
            b2: b0 / norm,
            a1: (-2.0 * cos_w0) / norm,
            a2: (1.0 - alpha) / norm,
        }
    }

    fn shared(cutoff: f32, rate: u32) -> (f32, f32, f32) {
        // Keep the cutoff strictly inside the band the rate can
        // represent; at or past Nyquist the coefficients stop being
        // finite.
        let nyquist = rate as f32 / 2.0;
        let cutoff = cutoff.clamp(10.0, nyquist * 0.98);
        let w0 = 2.0 * std::f32::consts::PI * cutoff / rate as f32;
        let alpha = w0.sin() / (2.0 * std::f32::consts::FRAC_1_SQRT_2);
        let cos_w0 = w0.cos();
        (cos_w0, alpha, 1.0 + alpha)
    }

    fn process(&self, samples: &mut [f32]) {
        let (mut x1, mut x2, mut y1, mut y2) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        for s in samples.iter_mut() {
            let x0 = *s;
            let y0 = self.b0 * x0 + self.b1 * x1 + self.b2 * x2 - self.a1 * y1 - self.a2 * y2;
            x2 = x1;
            x1 = x0;
            y2 = y1;
            y1 = y0;
            *s = y0;
        }
    }
}

/// Soft clip. `amount` is 0.0 (nothing) to 1.0 (hard enough to hear as
/// a radio), and the output is normalized back so driving a line does
/// not also make it louder.
pub fn drive(samples: &mut [f32], amount: f32) {
    let amount = amount.clamp(0.0, 1.0);
    if amount <= 0.0 {
        return;
    }
    let k = 1.0 + amount * 19.0;
    let norm = k.tanh();
    for s in samples.iter_mut() {
        *s = (*s * k).tanh() / norm;
    }
}

/// Everything a [`DirectionEffect`] does, in the order it has to happen.
///
/// The band limits are applied on *both* sides of the saturation, which
/// is the whole point of putting them together: `tanh` generates
/// harmonics, and a chain that filters and then saturates lets those
/// harmonics straight back out above the cutoff — measurably so, which
/// is how this was found. A real vox channel limits what its own
/// distortion produces, so the filters bracket it. With no drive the
/// second pass is skipped and this is one band-pass.
pub fn apply_effect(samples: &mut [f32], effect: &DirectionEffect, rate: u32) {
    // Two sections per edge. One is 12 dB/octave, which barely dents
    // the harmonics an octave up — measured at about -12 dB, audibly
    // not a radio. Cascading gets 24 dB/octave, which is close enough
    // to a channel to be worth the arithmetic. (Two Butterworth-Q
    // sections is not a 4th-order Butterworth — there is a little peak
    // at the corner — but a deliberately coloured channel is what this
    // is for, and the peak sits where a radio's does.)
    let band = |samples: &mut [f32]| {
        for _ in 0..2 {
            if let Some(hz) = effect.highpass_hz {
                Biquad::highpass(hz, rate).process(samples);
            }
            if let Some(hz) = effect.lowpass_hz {
                Biquad::lowpass(hz, rate).process(samples);
            }
        }
    };
    band(samples);
    if effect.drive > 0.0 {
        drive(samples, effect.drive);
        band(samples);
    }
    apply_gain(samples, effect.gain_db);
}

/// Peak level of the whole mix, in dBFS. `None` for silence.
pub fn peak_db(samples: &[f32]) -> Option<f32> {
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    (peak > 0.0).then(|| 20.0 * peak.log10())
}

/// Scale the mix so its peak sits at `target_db`. Returns the gain
/// applied, in dB.
///
/// Only ever used to bring a mix *down*: lines are summed at whatever
/// level the clone produced and a stacked `[SFX]` gap does not change
/// that, so a peak over the target means something clipped, while a
/// peak under it is just a quiet performance and raising it would undo
/// the levels the directions set.
pub fn limit_peak(samples: &mut [f32], target_db: f32) -> f32 {
    match peak_db(samples) {
        Some(peak) if peak > target_db => {
            apply_gain(samples, target_db - peak);
            target_db - peak
        }
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, hz: f32, secs: f32) -> Vec<f32> {
        let n = (rate as f32 * secs) as usize;
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin())
            .collect()
    }

    #[test]
    fn db_and_gain_agree() {
        assert!((db_to_gain(0.0) - 1.0).abs() < 1e-6);
        assert!((db_to_gain(-6.0) - 0.5012).abs() < 1e-3);
        assert!((db_to_gain(6.0) - 1.995).abs() < 1e-3);
    }

    #[test]
    fn gain_scales_and_is_free_at_zero() {
        let mut s = vec![0.5, -0.5];
        apply_gain(&mut s, 0.0);
        assert_eq!(s, vec![0.5, -0.5]);
        apply_gain(&mut s, -6.0206);
        assert!((s[0] - 0.25).abs() < 1e-3);
    }

    #[test]
    fn trims_padding_but_keeps_a_margin() {
        let rate = 24_000;
        let mut s = vec![0.0; rate as usize / 2]; // 500 ms of nothing
        s.extend(tone(rate, 440.0, 0.25));
        s.extend(vec![0.0; rate as usize / 2]);
        let keep = rate as usize / 100; // 10 ms
        let (start, end) = trim_silence(&s, -45.0, keep, rate);
        // The speech starts at 12000 and ends at 18000; the span should
        // sit just outside that on both sides.
        assert!(start < 12_000 && start > 12_000 - keep - 300, "{start}");
        assert!(end > 18_000 && end < 18_000 + keep + 300, "{end}");
    }

    #[test]
    fn a_silent_utterance_is_not_trimmed_to_nothing() {
        let s = vec![0.0f32; 4800];
        assert_eq!(trim_silence(&s, -45.0, 100, 24_000), (0, 4800));
    }

    #[test]
    fn fading_zeroes_both_edges_and_leaves_the_middle() {
        let mut s = vec![1.0f32; 100];
        fade_edges(&mut s, 10);
        assert_eq!(s[0], 0.0);
        assert_eq!(s[99], 0.0);
        assert_eq!(s[50], 1.0);
    }

    #[test]
    fn fading_a_buffer_shorter_than_the_fade_does_not_panic() {
        let mut s = vec![1.0f32; 3];
        fade_edges(&mut s, 1000);
        assert_eq!(s.len(), 3);
    }

    /// The thing an `(over vox)` band-pass is for: a tone inside the
    /// band survives and one below it does not.
    #[test]
    fn the_vox_bandpass_keeps_speech_and_drops_rumble() {
        let rate = 24_000;
        let effect = DirectionEffect {
            gain_db: 0.0,
            highpass_hz: Some(300.0),
            lowpass_hz: Some(3400.0),
            drive: 0.0,
        };
        for (hz, keeps) in [(60.0, false), (1000.0, true), (9000.0, false)] {
            let mut s = tone(rate, hz, 0.2);
            apply_effect(&mut s, &effect, rate);
            // Measure past the filter's settling time.
            let peak = peak_db(&s[rate as usize / 100..]).unwrap();
            if keeps {
                assert!(peak > -3.0, "{hz} Hz should survive, peak {peak}");
            } else {
                assert!(peak < -12.0, "{hz} Hz should be cut, peak {peak}");
            }
        }
    }

    /// What the chain order is for: with drive in the middle, the
    /// harmonics it makes have to be inside the band on the way out.
    #[test]
    fn saturation_does_not_leak_out_of_the_vox_band() {
        let rate = 24_000;
        let effect = DirectionEffect {
            gain_db: 0.0,
            highpass_hz: Some(300.0),
            lowpass_hz: Some(3400.0),
            drive: 0.35,
        };
        // 1 kHz driven hard makes odd harmonics at 3, 5, 7 kHz; only
        // the first of those is in the band.
        let mut s = tone(rate, 1000.0, 0.3);
        apply_effect(&mut s, &effect, rate);
        let settled = &s[rate as usize / 50..];

        let mut out_of_band = settled.to_vec();
        Biquad::highpass(6000.0, rate).process(&mut out_of_band);
        let mut in_band = settled.to_vec();
        Biquad::lowpass(2000.0, rate).process(&mut in_band);

        let leak = peak_db(&out_of_band).unwrap();
        let kept = peak_db(&in_band).unwrap();
        assert!(kept - leak > 20.0, "in band {kept}, above it {leak}");
    }

    #[test]
    fn drive_saturates_without_raising_the_peak() {
        let mut s = tone(24_000, 440.0, 0.1);
        let before = peak_db(&s).unwrap();
        drive(&mut s, 0.5);
        let after = peak_db(&s).unwrap();
        assert!(after <= before + 0.5, "before {before} after {after}");
        // A sine driven into tanh stops being a sine: its mean absolute
        // value rises toward a square's even though the peak does not.
        let mean = s.iter().map(|x| x.abs()).sum::<f32>() / s.len() as f32;
        assert!(mean > 0.75, "{mean}");
    }

    #[test]
    fn limiting_pulls_a_hot_mix_down_and_leaves_a_quiet_one_alone() {
        let mut hot = vec![2.0f32, -2.0];
        let applied = limit_peak(&mut hot, -1.0);
        assert!(applied < 0.0);
        assert!((peak_db(&hot).unwrap() - -1.0).abs() < 1e-3);

        let mut quiet = vec![0.1f32, -0.1];
        assert_eq!(limit_peak(&mut quiet, -1.0), 0.0);
        assert_eq!(quiet, vec![0.1, -0.1]);
    }

    #[test]
    fn a_filter_at_or_past_nyquist_stays_finite() {
        let rate = 24_000;
        let mut s = tone(rate, 1000.0, 0.05);
        let effect = DirectionEffect {
            gain_db: 0.0,
            highpass_hz: Some(0.0),
            lowpass_hz: Some(48_000.0),
            drive: 0.0,
        };
        apply_effect(&mut s, &effect, rate);
        assert!(s.iter().all(|x| x.is_finite()));
    }
}
