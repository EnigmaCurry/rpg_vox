//! Speech-to-text via [sherpa-onnx].
//!
//! One offline, non-streaming recognizer instance for the whole process,
//! loaded lazily on the first `/widgets/record/:sid/stop` that has samples
//! to transcribe. Held as a `Send + Sync` handle inside the HTTP AppState so
//! any request can call [`SttHandle::transcribe`] without extra locking.
//!
//! Model choice is fixed to the multilingual SenseVoice model — one bundle
//! covers en/zh/ja/ko/yue and is small (~230 MB int8) with high accuracy
//! for widget-length clips, which is what our recording flow produces. We
//! feed the recognizer the raw 48 kHz mono PCM captured off the `-vox`
//! sink; sherpa-onnx's feature extractor resamples to 16 kHz internally
//! (SenseVoice's native rate), so no upfront rubato pass is needed.
//!
//! When the configured model/tokens paths are missing at startup we skip
//! loading and log a warning — the record flow still works, it just won't
//! auto-fill the SpeakCell text with a transcript.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow};
use sherpa_onnx::{
    OfflineRecognizer, OfflineRecognizerConfig, OfflineSenseVoiceModelConfig,
};
use tracing::{info, warn};

/// User-facing knobs (surfaced via CLI + env). All paths are absolute or
/// relative to CWD at startup; the loader errors out on missing files so a
/// typo doesn't silently give the user an empty transcript later.
#[derive(Clone, Debug)]
pub struct SttConfig {
    /// Path to `model.int8.onnx` (or the fp32 variant). None → STT disabled.
    pub model: Option<PathBuf>,
    /// Path to the model's `tokens.txt`. Must be paired with `model`.
    pub tokens: Option<PathBuf>,
    /// SenseVoice language hint. `"auto"` (default) lets the model detect;
    /// alternatives are `"en"`, `"zh"`, `"ja"`, `"ko"`, `"yue"`.
    pub language: String,
    /// Threads sherpa-onnx uses for feature extraction + inference. Two
    /// is a decent default for widget-sized clips on a desktop; larger
    /// clips can bump this up.
    pub num_threads: i32,
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            model: None,
            tokens: None,
            language: "auto".to_string(),
            num_threads: 2,
        }
    }
}

/// Cloneable handle to the loaded recognizer. `OfflineRecognizer` is
/// `Send + Sync` per the crate docs, so cloning the `Arc` and calling
/// `transcribe` concurrently from multiple request handlers is safe.
#[derive(Clone)]
pub struct SttHandle {
    inner: Arc<OfflineRecognizer>,
}

impl SttHandle {
    /// Load the recognizer from disk. Returns `Ok(None)` when both paths
    /// are unset (STT explicitly disabled). An `Err` means the paths were
    /// provided but something went wrong loading — surfaced at startup so
    /// misconfiguration is obvious rather than deferred to first use.
    pub fn open(cfg: &SttConfig) -> Result<Option<Self>> {
        let (model, tokens) = match (&cfg.model, &cfg.tokens) {
            (Some(m), Some(t)) => (m, t),
            (None, None) => {
                info!("STT disabled (no --stt-model / --stt-tokens configured)");
                return Ok(None);
            }
            (Some(_), None) | (None, Some(_)) => {
                return Err(anyhow!(
                    "STT model and tokens must be provided together (got one but not the other)"
                ));
            }
        };
        if !model.is_file() {
            return Err(anyhow!("STT model file not found: {}", model.display()));
        }
        if !tokens.is_file() {
            return Err(anyhow!("STT tokens file not found: {}", tokens.display()));
        }

        let mut config = OfflineRecognizerConfig::default();
        config.model_config.sense_voice = OfflineSenseVoiceModelConfig {
            model: Some(model.to_string_lossy().into_owned()),
            language: Some(cfg.language.clone()),
            use_itn: true,
        };
        config.model_config.tokens = Some(tokens.to_string_lossy().into_owned());
        config.model_config.num_threads = cfg.num_threads;

        let recognizer = OfflineRecognizer::create(&config)
            .ok_or_else(|| anyhow!("sherpa-onnx failed to create recognizer (bad model?)"))?;
        info!(
            model = %model.display(),
            language = %cfg.language,
            "STT recognizer loaded (SenseVoice)"
        );
        Ok(Some(Self { inner: Arc::new(recognizer) }))
    }

    /// Run recognition on a mono f32 PCM buffer. `sample_rate` is what the
    /// caller *sampled at* — sherpa-onnx resamples internally to the
    /// model's expected rate. Returns the raw text; empty string means the
    /// clip decoded to nothing (silence / non-speech).
    pub fn transcribe(&self, samples: &[f32], sample_rate: u32) -> Result<String> {
        // Very short clips (< ~50 ms) tend to produce garbage transcripts
        // from any ASR; short-circuit to empty rather than emit noise.
        if samples.len() < (sample_rate as usize / 20).max(64) {
            return Ok(String::new());
        }
        let stream = self.inner.create_stream();
        stream.accept_waveform(sample_rate as i32, samples);
        self.inner.decode(&stream);
        let result = stream
            .get_result()
            .context("sherpa-onnx returned no result")?;
        Ok(result.text.trim().to_string())
    }
}

/// Optional convenience for `main.rs`: load the handle, or log a warning
/// and return `None` when loading fails but a path was configured. Keeps
/// startup non-fatal even on model breakage, since the record flow is
/// still usable without STT (the user can type the caption manually).
pub fn open_or_warn(cfg: &SttConfig) -> Option<SttHandle> {
    match SttHandle::open(cfg) {
        Ok(handle) => handle,
        Err(err) => {
            warn!(err = %format!("{err:#}"), "STT disabled — failed to load recognizer");
            None
        }
    }
}
