//! sherpa-onnx recognizers: streaming Zipformer transducer for pass 1,
//! offline SenseVoice or NeMo Parakeet TDT for passes 2 and 3.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context as _, Result};
use sherpa_onnx::{
    OfflineRecognizerConfig, OfflineSenseVoiceModelConfig, OfflineTransducerModelConfig,
    OnlineRecognizerConfig, OnlineStream,
};
use tracing::info;

use crate::recognizer::{OfflineRecognizer, StreamingRecognizer};

fn require(p: &Path, label: &str) -> Result<String> {
    if !p.is_file() {
        return Err(anyhow!("{label} file not found: {}", p.display()));
    }
    Ok(p.to_string_lossy().into_owned())
}

/// SenseVoice bundle: `model.int8.onnx` + `tokens.txt`.
#[derive(Clone, Debug)]
pub struct SenseVoiceConfig {
    pub model: PathBuf,
    pub tokens: PathBuf,
    /// `"auto"`, `"en"`, `"zh"`, `"ja"`, `"ko"` or `"yue"`.
    pub language: String,
    pub num_threads: i32,
}

impl SenseVoiceConfig {
    /// Layout written by `just download-stt-model`: `<dir>/model.int8.onnx`.
    pub fn from_dir(dir: &Path) -> Self {
        Self {
            model: dir.join("model.int8.onnx"),
            tokens: dir.join("tokens.txt"),
            language: "auto".into(),
            num_threads: 2,
        }
    }
}

pub struct SenseVoice {
    inner: sherpa_onnx::OfflineRecognizer,
}

impl SenseVoice {
    pub fn open(cfg: &SenseVoiceConfig) -> Result<Self> {
        let mut config = OfflineRecognizerConfig::default();
        config.model_config.sense_voice = OfflineSenseVoiceModelConfig {
            model: Some(require(&cfg.model, "SenseVoice model")?),
            language: Some(cfg.language.clone()),
            use_itn: true,
        };
        config.model_config.tokens = Some(require(&cfg.tokens, "SenseVoice tokens")?);
        config.model_config.num_threads = cfg.num_threads;
        let inner = sherpa_onnx::OfflineRecognizer::create(&config)
            .ok_or_else(|| anyhow!("sherpa-onnx failed to create SenseVoice recognizer"))?;
        info!(model = %cfg.model.display(), language = %cfg.language, "SenseVoice loaded");
        Ok(Self { inner })
    }
}

impl OfflineRecognizer for SenseVoice {
    fn transcribe(&self, samples: &[f32], sample_rate: u32) -> Result<String> {
        decode_offline(&self.inner, samples, sample_rate)
    }
}

/// NeMo Parakeet TDT bundle (`sherpa-onnx-nemo-parakeet-tdt-*-int8`):
/// `encoder.int8.onnx`, `decoder.int8.onnx`, `joiner.int8.onnx`,
/// `tokens.txt`. Punctuates and capitalises on its own.
#[derive(Clone, Debug)]
pub struct ParakeetConfig {
    pub encoder: PathBuf,
    pub decoder: PathBuf,
    pub joiner: PathBuf,
    pub tokens: PathBuf,
    pub num_threads: i32,
}

impl ParakeetConfig {
    pub fn from_dir(dir: &Path) -> Self {
        Self {
            encoder: dir.join("encoder.int8.onnx"),
            decoder: dir.join("decoder.int8.onnx"),
            joiner: dir.join("joiner.int8.onnx"),
            tokens: dir.join("tokens.txt"),
            num_threads: 2,
        }
    }
}

pub struct Parakeet {
    inner: sherpa_onnx::OfflineRecognizer,
}

impl Parakeet {
    pub fn open(cfg: &ParakeetConfig) -> Result<Self> {
        let mut config = OfflineRecognizerConfig::default();
        config.model_config.transducer = OfflineTransducerModelConfig {
            encoder: Some(require(&cfg.encoder, "Parakeet encoder")?),
            decoder: Some(require(&cfg.decoder, "Parakeet decoder")?),
            joiner: Some(require(&cfg.joiner, "Parakeet joiner")?),
        };
        config.model_config.tokens = Some(require(&cfg.tokens, "Parakeet tokens")?);
        config.model_config.model_type = Some("nemo_transducer".into());
        config.model_config.num_threads = cfg.num_threads;
        let inner = sherpa_onnx::OfflineRecognizer::create(&config)
            .ok_or_else(|| anyhow!("sherpa-onnx failed to create Parakeet recognizer"))?;
        info!(encoder = %cfg.encoder.display(), "Parakeet loaded");
        Ok(Self { inner })
    }
}

impl OfflineRecognizer for Parakeet {
    fn transcribe(&self, samples: &[f32], sample_rate: u32) -> Result<String> {
        decode_offline(&self.inner, samples, sample_rate)
    }
}

fn decode_offline(
    recognizer: &sherpa_onnx::OfflineRecognizer,
    samples: &[f32],
    sample_rate: u32,
) -> Result<String> {
    // Clips under ~50 ms decode to garbage on any ASR.
    if samples.len() < (sample_rate as usize / 20).max(64) {
        return Ok(String::new());
    }
    let stream = recognizer.create_stream();
    stream.accept_waveform(sample_rate as i32, samples);
    recognizer.decode(&stream);
    let result = stream
        .get_result()
        .context("sherpa-onnx returned no result")?;
    Ok(result.text.trim().to_string())
}

/// Streaming Zipformer transducer bundle.
#[derive(Clone, Debug)]
pub struct ZipformerConfig {
    pub encoder: PathBuf,
    pub decoder: PathBuf,
    pub joiner: PathBuf,
    pub tokens: PathBuf,
    pub num_threads: i32,
}

impl ZipformerConfig {
    /// Layout written by `just download-streaming-stt-model`.
    pub fn from_dir(dir: &Path) -> Self {
        Self {
            encoder: dir.join("encoder.onnx"),
            decoder: dir.join("decoder.onnx"),
            joiner: dir.join("joiner.onnx"),
            tokens: dir.join("tokens.txt"),
            num_threads: 2,
        }
    }
}

/// Shared loaded model; call [`Zipformer::session`] once per channel.
#[derive(Clone)]
pub struct Zipformer {
    inner: Arc<sherpa_onnx::OnlineRecognizer>,
}

impl Zipformer {
    pub fn open(cfg: &ZipformerConfig) -> Result<Self> {
        let mut config = OnlineRecognizerConfig::default();
        config.model_config.transducer.encoder = Some(require(&cfg.encoder, "Zipformer encoder")?);
        config.model_config.transducer.decoder = Some(require(&cfg.decoder, "Zipformer decoder")?);
        config.model_config.transducer.joiner = Some(require(&cfg.joiner, "Zipformer joiner")?);
        config.model_config.tokens = Some(require(&cfg.tokens, "Zipformer tokens")?);
        config.model_config.num_threads = cfg.num_threads;
        config.decoding_method = Some("greedy_search".into());
        // The VAD owns utterance boundaries; sherpa's endpointer is only a
        // safety net. Values match the sherpa demo defaults.
        config.enable_endpoint = true;
        config.rule1_min_trailing_silence = 2.4;
        config.rule2_min_trailing_silence = 1.2;
        config.rule3_min_utterance_length = 20.0;
        let inner = sherpa_onnx::OnlineRecognizer::create(&config)
            .ok_or_else(|| anyhow!("sherpa-onnx failed to create streaming recognizer"))?;
        info!(encoder = %cfg.encoder.display(), "Zipformer loaded");
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    pub fn session(&self) -> ZipformerSession {
        ZipformerSession {
            stream: self.inner.create_stream(),
            recognizer: self.inner.clone(),
        }
    }
}

pub struct ZipformerSession {
    recognizer: Arc<sherpa_onnx::OnlineRecognizer>,
    stream: OnlineStream,
}

impl StreamingRecognizer for ZipformerSession {
    fn reset(&mut self) {
        self.recognizer.reset(&self.stream);
    }

    fn feed(&mut self, samples: &[f32], sample_rate: u32) {
        if samples.is_empty() {
            return;
        }
        self.stream.accept_waveform(sample_rate as i32, samples);
        while self.recognizer.is_ready(&self.stream) {
            self.recognizer.decode(&self.stream);
        }
    }

    fn partial(&self) -> String {
        self.recognizer
            .get_result(&self.stream)
            .map(|r| r.text.trim().to_string())
            .unwrap_or_default()
    }
}
