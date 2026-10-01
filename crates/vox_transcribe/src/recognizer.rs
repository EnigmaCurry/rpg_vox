//! Recognizer traits. The engine only talks to these, so it can be
//! driven by fakes in tests and by other STT backends later.

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

/// Pass-2/3 high-accuracy recognizer over a complete buffer.
pub trait OfflineRecognizer: Send + Sync {
    fn transcribe(&self, samples: &[f32], sample_rate: u32) -> anyhow::Result<String>;
}
