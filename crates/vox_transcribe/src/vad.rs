//! Energy-based voice activity detector.
//!
//! Runs at window granularity: each window covers `window_ms` of mono
//! audio and contributes its RMS to a speaking/silent state machine.
//! Per-sample checks don't work for speech, which crosses zero many
//! times per cycle.

use std::collections::VecDeque;

#[derive(Debug, Clone)]
pub struct VadConfig {
    pub window_ms: u32,
    /// RMS threshold in linear amplitude. Normal talking sits around
    /// 0.04–0.15; residual digital silence is well under 0.001.
    pub rms_threshold: f32,
    /// Consecutive voiced time needed to enter Speaking. Filters one-off
    /// spikes (a keyboard click); pre-roll recovers the first syllable.
    pub speech_start_ms: u32,
    /// Consecutive silence needed to close an utterance.
    pub silence_end_ms: u32,
    /// Audio kept from before the Speaking trigger.
    pub pre_roll_ms: u32,
    /// Past this length, close the utterance at the next short pause
    /// (`pause_split_ms`) instead of waiting for `silence_end_ms`, so
    /// continuous back-and-forth speech is cut between words rather than
    /// by `max_utterance_ms`. Offline recognizers also drop whole
    /// sentences from long buffers (Parakeet did at 15 s).
    pub soft_max_utterance_ms: u32,
    /// Quiet needed to split an utterance past `soft_max_utterance_ms`.
    pub pause_split_ms: u32,
    /// Force-close an utterance that runs this long (hot mic, steady noise).
    pub max_utterance_ms: u32,
    /// Utterances shorter than this are discarded as noise.
    pub min_utterance_ms: u32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            window_ms: 20,
            rms_threshold: 0.008,
            speech_start_ms: 150,
            silence_end_ms: 700,
            pre_roll_ms: 300,
            soft_max_utterance_ms: 8_000,
            pause_split_ms: 60,
            max_utterance_ms: 12_000,
            min_utterance_ms: 200,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    Silence,
    /// A short pause past the soft maximum length.
    Pause,
    MaxLength,
    Flush,
}

#[derive(Debug)]
pub enum VadEvent {
    /// Speech started. `audio` is the pre-roll plus the triggering
    /// window; `start_sample` is the position of its first sample.
    Start { start_sample: u64, audio: Vec<f32> },
    /// One more window of the current utterance.
    Audio(Vec<f32>),
    /// Utterance closed. `samples` is the whole utterance including
    /// everything already sent via `Start` and `Audio`.
    End {
        start_sample: u64,
        samples: Vec<f32>,
        reason: EndReason,
    },
    /// Utterance closed but was too short to be speech.
    Discard,
}

pub struct Vad {
    cfg: VadConfig,
    window_samples: usize,
    pre_roll_cap: usize,
    speech_start_windows: u32,
    silence_end_windows: u32,
    pause_windows: u32,
    soft_max_samples: usize,
    max_samples: usize,
    min_samples: usize,
    scratch: Vec<f32>,
    utterance: Vec<f32>,
    pre_roll: VecDeque<f32>,
    speaking: bool,
    voiced_windows: u32,
    silence_windows: u32,
    /// Samples consumed into complete windows so far.
    pos: u64,
    utterance_start: u64,
    last_rms: f32,
}

impl Vad {
    pub fn new(cfg: VadConfig, sample_rate: u32) -> Self {
        let ms = |v: u32| (sample_rate as u64 * v as u64 / 1000) as usize;
        Self {
            window_samples: ms(cfg.window_ms).max(1),
            pre_roll_cap: ms(cfg.pre_roll_ms),
            speech_start_windows: (cfg.speech_start_ms / cfg.window_ms).max(1),
            silence_end_windows: (cfg.silence_end_ms / cfg.window_ms).max(1),
            pause_windows: (cfg.pause_split_ms / cfg.window_ms).max(1),
            soft_max_samples: ms(cfg.soft_max_utterance_ms),
            max_samples: ms(cfg.max_utterance_ms),
            min_samples: ms(cfg.min_utterance_ms),
            scratch: Vec::new(),
            utterance: Vec::new(),
            pre_roll: VecDeque::new(),
            speaking: false,
            voiced_windows: 0,
            silence_windows: 0,
            pos: 0,
            utterance_start: 0,
            last_rms: 0.0,
            cfg,
        }
    }

    pub fn is_speaking(&self) -> bool {
        self.speaking
    }

    /// RMS of the most recent window.
    pub fn last_rms(&self) -> f32 {
        self.last_rms
    }

    /// Samples consumed so far (the engine's audio clock).
    pub fn position(&self) -> u64 {
        self.pos
    }

    pub fn push(&mut self, mono: &[f32], out: &mut Vec<VadEvent>) {
        self.scratch.extend_from_slice(mono);
        let mut offset = 0;
        while self.scratch.len() - offset >= self.window_samples {
            let window = self.scratch[offset..offset + self.window_samples].to_vec();
            offset += self.window_samples;
            self.process_window(window, out);
        }
        self.scratch.drain(..offset);
    }

    /// Close any in-flight utterance (end of stream, or a forced break).
    pub fn flush(&mut self, out: &mut Vec<VadEvent>) {
        if self.speaking {
            self.finalize(EndReason::Flush, out);
        }
    }

    fn process_window(&mut self, window: Vec<f32>, out: &mut Vec<VadEvent>) {
        self.pos += window.len() as u64;
        let rms = (window.iter().map(|s| s * s).sum::<f32>() / window.len() as f32).sqrt();
        self.last_rms = rms;
        let hot = rms > self.cfg.rms_threshold;

        if self.speaking {
            self.utterance.extend_from_slice(&window);
            out.push(VadEvent::Audio(window));
            if hot {
                self.silence_windows = 0;
            } else {
                self.silence_windows += 1;
                if self.silence_windows >= self.silence_end_windows {
                    self.finalize(EndReason::Silence, out);
                    return;
                }
                if self.utterance.len() >= self.soft_max_samples
                    && self.silence_windows >= self.pause_windows
                {
                    self.finalize(EndReason::Pause, out);
                    return;
                }
            }
            if self.utterance.len() >= self.max_samples {
                self.finalize(EndReason::MaxLength, out);
            }
            return;
        }

        for &s in &window {
            if self.pre_roll.len() >= self.pre_roll_cap {
                self.pre_roll.pop_front();
            }
            if self.pre_roll_cap > 0 {
                self.pre_roll.push_back(s);
            }
        }
        if !hot {
            self.voiced_windows = 0;
            return;
        }
        self.voiced_windows += 1;
        if self.voiced_windows < self.speech_start_windows {
            return;
        }
        self.speaking = true;
        self.silence_windows = 0;
        self.utterance.clear();
        self.utterance.extend(self.pre_roll.drain(..));
        if self.pre_roll_cap == 0 {
            self.utterance.extend_from_slice(&window);
        }
        self.utterance_start = self.pos.saturating_sub(self.utterance.len() as u64);
        out.push(VadEvent::Start {
            start_sample: self.utterance_start,
            audio: self.utterance.clone(),
        });
    }

    fn finalize(&mut self, reason: EndReason, out: &mut Vec<VadEvent>) {
        let samples = std::mem::take(&mut self.utterance);
        self.speaking = false;
        self.silence_windows = 0;
        self.voiced_windows = 0;
        self.pre_roll.clear();
        if samples.len() < self.min_samples {
            out.push(VadEvent::Discard);
        } else {
            out.push(VadEvent::End {
                start_sample: self.utterance_start,
                samples,
                reason,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 16_000;

    fn tone(ms: u32, amp: f32) -> Vec<f32> {
        let n = (SR * ms / 1000) as usize;
        (0..n).map(|i| amp * (i as f32 * 0.3).sin()).collect()
    }

    #[test]
    fn detects_one_utterance_with_preroll() {
        let mut vad = Vad::new(VadConfig::default(), SR);
        let mut ev = Vec::new();
        vad.push(&tone(1000, 0.0), &mut ev);
        vad.push(&tone(1000, 0.2), &mut ev);
        vad.push(&tone(1000, 0.0), &mut ev);
        let starts: Vec<u64> = ev
            .iter()
            .filter_map(|e| match e {
                VadEvent::Start { start_sample, .. } => Some(*start_sample),
                _ => None,
            })
            .collect();
        assert_eq!(starts.len(), 1);
        // Trigger after 7 voiced windows (1140 ms), minus 300 ms pre-roll.
        assert_eq!(starts[0], (SR as u64) * 840 / 1000);
        let ends: Vec<_> = ev
            .iter()
            .filter_map(|e| match e {
                VadEvent::End {
                    samples, reason, ..
                } => Some((samples.len(), *reason)),
                _ => None,
            })
            .collect();
        assert_eq!(ends.len(), 1);
        assert_eq!(ends[0].1, EndReason::Silence);
        // 300 pre-roll + 860 speech + 700 silence.
        assert_eq!(ends[0].0, (SR as usize) * 1860 / 1000);
    }

    #[test]
    fn short_blip_is_ignored() {
        let mut vad = Vad::new(VadConfig::default(), SR);
        let mut ev = Vec::new();
        vad.push(&tone(100, 0.0), &mut ev);
        vad.push(&tone(60, 0.2), &mut ev);
        vad.push(&tone(1000, 0.0), &mut ev);
        assert!(ev.is_empty());
    }

    #[test]
    fn max_length_rolls_over() {
        let cfg = VadConfig {
            max_utterance_ms: 2_000,
            ..Default::default()
        };
        let mut vad = Vad::new(cfg, SR);
        let mut ev = Vec::new();
        vad.push(&tone(5000, 0.2), &mut ev);
        vad.flush(&mut ev);
        let reasons: Vec<_> = ev
            .iter()
            .filter_map(|e| match e {
                VadEvent::End { reason, .. } => Some(*reason),
                _ => None,
            })
            .collect();
        assert_eq!(reasons[0], EndReason::MaxLength);
        assert_eq!(*reasons.last().unwrap(), EndReason::Flush);
    }

    #[test]
    fn long_utterance_splits_at_a_short_pause() {
        let mut vad = Vad::new(VadConfig::default(), SR);
        let mut ev = Vec::new();
        // Speech with 100 ms dips: no 700 ms silence, but past 8 s the
        // first dip closes the utterance.
        for _ in 0..12 {
            vad.push(&tone(900, 0.2), &mut ev);
            vad.push(&tone(100, 0.0), &mut ev);
        }
        vad.flush(&mut ev);
        let ends: Vec<_> = ev
            .iter()
            .filter_map(|e| match e {
                VadEvent::End {
                    samples, reason, ..
                } => Some((samples.len() as u64 * 1000 / SR as u64, *reason)),
                _ => None,
            })
            .collect();
        assert_eq!(ends[0].1, EndReason::Pause);
        assert!(ends[0].0 >= 8_000 && ends[0].0 < 9_000, "{ends:?}");
        assert!(ends.iter().all(|(_, r)| *r != EndReason::MaxLength));
    }
}
