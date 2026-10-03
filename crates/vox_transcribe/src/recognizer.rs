//! Recognizer traits. The engine only talks to these, so it can be
//! driven by fakes in tests and by other STT backends later.

use crate::timing::Word;

/// Pass-1 low-latency recognizer. One instance per audio channel; the
/// engine calls it from a single thread.
pub trait StreamingRecognizer: Send {
    /// Drop the current utterance's decode state.
    fn reset(&mut self);
    /// Feed more audio of the current utterance and decode what's ready.
    fn feed(&mut self, samples: &[f32], sample_rate: u32);
    /// Current partial hypothesis, trimmed.
    fn partial(&self) -> String;
}

/// Text of a decode plus, when the recognizer reports them, its word
/// timings in ms from the start of the buffer.
#[derive(Debug, Clone, Default)]
pub struct Transcription {
    pub text: String,
    pub words: Vec<Word>,
}

/// Pass-2/3 high-accuracy recognizer over a complete buffer.
pub trait OfflineRecognizer: Send + Sync {
    fn transcribe(&self, samples: &[f32], sample_rate: u32) -> anyhow::Result<String>;

    /// Like [`OfflineRecognizer::transcribe`], with word timings. The
    /// default has none; the engine then spreads words evenly over the
    /// clip.
    fn transcribe_timed(&self, samples: &[f32], sample_rate: u32) -> anyhow::Result<Transcription> {
        Ok(Transcription {
            text: self.transcribe(samples, sample_rate)?,
            words: Vec::new(),
        })
    }
}

/// Live speaker identification: labels each finished utterance with a
/// speaker ("A", "B", …). Called on the offline worker thread, one
/// utterance at a time and in order, so implementations may keep state.
pub trait SpeakerTagger: Send {
    /// `None` when the utterance is too short or unclear to tell.
    fn identify(&mut self, samples: &[f32], sample_rate: u32) -> Option<String>;
}
