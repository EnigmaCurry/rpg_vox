//! Piper TTS backend (in-process, via the `piper-rs` crate).
//!
//! The ONNX model + config JSON are loaded **once at startup**. Each phrase
//! runs on the tokio blocking pool so we don't stall the async executor;
//! synthesis is serialized by a mutex, which is fine since we only speak one
//! phrase at a time anyway.
//!
//! Piper emits a complete mono `Vec<f32>` per `create()` call at the model's
//! native rate (22050 Hz for the medium voices). The shared [`AudioChunk`] the
//! runner consumes carries the raw PCM plus that native rate — resampling to
//! the pipewire target lives in the runner.

use anyhow::{Context, Result, anyhow};
use piper_rs::Piper;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::debug;

use super::{AudioChunk, Sink, chunker};

pub struct Config {
    pub model_path: String,
    pub config_path: String,
    /// Multi-speaker voices need this; single-speaker models ignore it.
    pub speaker_id: Option<i64>,
    /// Piper duration multiplier. `None` uses the model's default (usually 1.0);
    /// values > 1.0 slow speech down, < 1.0 speed it up.
    pub length_scale: Option<f32>,
    pub chunker: chunker::Config,
    pub pauses: PauseConfig,
}

/// Silence inserted between phrases so back-to-back Piper outputs get natural
/// cadence. Sized by the punctuation the phrase ended with; a phrase that
/// force-broke on word count gets the shortest gap.
pub struct PauseConfig {
    pub strong_ms: u32,
    pub weak_ms: u32,
    pub force_ms: u32,
}

impl Default for PauseConfig {
    fn default() -> Self {
        Self {
            strong_ms: 320,
            weak_ms: 140,
            force_ms: 40,
        }
    }
}

fn pause_ms_for(phrase: &str, cfg: &PauseConfig) -> u32 {
    let trimmed = phrase.trim_end_matches(|c: char| {
        c.is_whitespace() || c == '"' || c == '\'' || c == ')' || c == ']'
    });
    match trimmed.chars().last() {
        Some('.') | Some('?') | Some('!') => cfg.strong_ms,
        Some(',') | Some(':') | Some(';') => cfg.weak_ms,
        _ => cfg.force_ms,
    }
}

pub struct Backend {
    /// Wrapped in a Mutex because `Piper::create` is `&mut self` and we hand it
    /// to `spawn_blocking`. Contention is a non-issue: we serialize speech
    /// anyway.
    piper: Arc<Mutex<Piper>>,
    speaker_id: Option<i64>,
    length_scale: Option<f32>,
    chunker: chunker::Config,
    pauses: PauseConfig,
}

impl Backend {
    pub fn load(cfg: Config) -> Result<Self> {
        let model_path = Path::new(&cfg.model_path);
        let config_path = Path::new(&cfg.config_path);
        if !model_path.exists() {
            return Err(anyhow!(
                "piper model file not found: {}",
                model_path.display()
            ));
        }
        if !config_path.exists() {
            return Err(anyhow!(
                "piper config file not found: {}",
                config_path.display()
            ));
        }
        let piper = Piper::new(model_path, config_path)
            .map_err(|e| anyhow!("{e}"))
            .with_context(|| {
                format!(
                    "loading piper model {} (config {})",
                    model_path.display(),
                    config_path.display()
                )
            })?;
        Ok(Self {
            piper: Arc::new(Mutex::new(piper)),
            speaker_id: cfg.speaker_id,
            length_scale: cfg.length_scale,
            chunker: cfg.chunker,
            pauses: cfg.pauses,
        })
    }

    /// Chunk `text` into short phrases and push each phrase's PCM to `sink`
    /// as soon as it's ready. First-phrase latency is what matters here —
    /// audio starts playing while later phrases are still being synthesized.
    pub async fn synthesize(&mut self, text: &str, sink: &mut Sink<'_>) -> Result<()> {
        let phrases = chunker::chunk(text, &self.chunker);
        let last = phrases.len().saturating_sub(1);
        for (i, phrase) in phrases.iter().enumerate() {
            let chunk = self.synthesize_phrase(phrase).await?;
            let rate = chunk.sample_rate;
            debug!(
                phrase_idx = i,
                phrase_len = phrase.len(),
                samples = chunk.samples.len(),
                rate,
                "piper phrase synthesized"
            );
            sink.push(chunk).await?;
            // Insert a punctuation-shaped pause between phrases so back-to-back
            // syntheses don't run together. Skip after the last phrase — the
            // sink's own drain is silence enough.
            if i < last {
                let ms = pause_ms_for(phrase, &self.pauses);
                if ms > 0 {
                    let n = (rate as u64 * ms as u64 / 1000) as usize;
                    sink.push(AudioChunk {
                        samples: vec![0.0f32; n],
                        sample_rate: rate,
                    })
                    .await?;
                }
            }
        }
        Ok(())
    }

    pub async fn synthesize_phrase(&mut self, phrase: &str) -> Result<AudioChunk> {
        let piper = self.piper.clone();
        let speaker_id = self.speaker_id;
        let length_scale = self.length_scale;
        let phrase = phrase.to_string();
        let (samples, sample_rate) =
            tokio::task::spawn_blocking(move || -> Result<(Vec<f32>, u32)> {
                let mut p = piper
                    .lock()
                    .map_err(|_| anyhow!("piper mutex poisoned"))?;
                p.create(&phrase, false, speaker_id, length_scale, None, None)
                    .map_err(|e| anyhow!("piper synth: {e}"))
            })
            .await
            .context("piper spawn_blocking join")??;
        Ok(AudioChunk {
            samples,
            sample_rate,
        })
    }
}
