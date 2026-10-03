//! sherpa-onnx recognizers: streaming Zipformer transducer for pass 1,
//! offline SenseVoice or NeMo Parakeet TDT for passes 2 and 3, plus
//! speaker embeddings (live labels) and pyannote diarization (offline).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context as _, Result};
use std::ffi::{c_void, CString};

use sherpa_onnx::{
    LinearResampler, OfflineRecognizerConfig, OfflineSenseVoiceModelConfig,
    OfflineTransducerModelConfig, OnlineRecognizerConfig, OnlineStream,
    SpeakerEmbeddingExtractorConfig,
};
use sherpa_onnx_sys as sys;
use tracing::info;

use crate::diarize::Segment;
use crate::recognizer::{OfflineRecognizer, SpeakerTagger, StreamingRecognizer, Transcription};
use crate::speaker::{label, ClusterConfig, OnlineClusters};
use crate::timing::words_from_tokens;

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
        Ok(decode_offline(&self.inner, samples, sample_rate)?.text)
    }

    fn transcribe_timed(&self, samples: &[f32], sample_rate: u32) -> Result<Transcription> {
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
        Ok(decode_offline(&self.inner, samples, sample_rate)?.text)
    }

    fn transcribe_timed(&self, samples: &[f32], sample_rate: u32) -> Result<Transcription> {
        decode_offline(&self.inner, samples, sample_rate)
    }
}

fn decode_offline(
    recognizer: &sherpa_onnx::OfflineRecognizer,
    samples: &[f32],
    sample_rate: u32,
) -> Result<Transcription> {
    // Clips under ~50 ms decode to garbage on any ASR.
    if samples.len() < (sample_rate as usize / 20).max(64) {
        return Ok(Transcription::default());
    }
    let stream = recognizer.create_stream();
    stream.accept_waveform(sample_rate as i32, samples);
    recognizer.decode(&stream);
    let result = stream
        .get_result()
        .context("sherpa-onnx returned no result")?;
    let total_ms = samples.len() as u64 * 1000 / sample_rate.max(1) as u64;
    let words = words_from_tokens(
        &result.tokens,
        result.timestamps.as_deref().unwrap_or_default(),
        result.durations.as_deref().unwrap_or_default(),
        total_ms,
    );
    Ok(Transcription {
        text: result.text.trim().to_string(),
        words,
    })
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

/// Rate the speaker models run at.
const SPEAKER_RATE: u32 = 16_000;

/// `samples` converted from rate `from` to rate `to`.
fn resample(samples: &[f32], from: u32, to: u32) -> Result<Vec<f32>> {
    if from == to {
        return Ok(samples.to_vec());
    }
    let r = LinearResampler::create(from as i32, to as i32)
        .ok_or_else(|| anyhow!("sherpa-onnx failed to create a resampler"))?;
    Ok(r.resample(samples, true))
}

/// Speaker models: pyannote segmentation (offline diarization only) and
/// a speaker-embedding model (both modes).
#[derive(Clone, Debug)]
pub struct SpeakerModels {
    pub segmentation: PathBuf,
    pub embedding: PathBuf,
    pub num_threads: i32,
}

impl SpeakerModels {
    /// Layout written by `scribe download-models --diarize`:
    /// `<dir>/segmentation.onnx` and `<dir>/embedding.onnx`.
    pub fn from_dir(dir: &Path) -> Self {
        Self {
            segmentation: dir.join("segmentation.onnx"),
            embedding: dir.join("embedding.onnx"),
            num_threads: 2,
        }
    }

    fn embedding_config(&self) -> Result<SpeakerEmbeddingExtractorConfig> {
        Ok(SpeakerEmbeddingExtractorConfig {
            model: Some(require(&self.embedding, "speaker embedding model")?),
            num_threads: self.num_threads,
            ..Default::default()
        })
    }
}

/// Voice embeddings of arbitrary stretches of audio.
pub struct SpeakerEmbedder {
    extractor: sherpa_onnx::SpeakerEmbeddingExtractor,
}

impl SpeakerEmbedder {
    pub fn open(models: &SpeakerModels) -> Result<Self> {
        let extractor = sherpa_onnx::SpeakerEmbeddingExtractor::create(&models.embedding_config()?)
            .ok_or_else(|| anyhow!("sherpa-onnx failed to create speaker embedding extractor"))?;
        info!(model = %models.embedding.display(), dim = extractor.dim(), "speaker embeddings loaded");
        Ok(Self { extractor })
    }

    /// The embedding of `samples` (mono at `sample_rate`), or `None` when
    /// too short to compute.
    pub fn embed(&self, samples: &[f32], sample_rate: u32) -> Option<Vec<f32>> {
        let audio = resample(samples, sample_rate, SPEAKER_RATE).ok()?;
        let stream = self.extractor.create_stream()?;
        stream.accept_waveform(SPEAKER_RATE as i32, &audio);
        stream.input_finished();
        if !self.extractor.is_ready(&stream) {
            return None;
        }
        self.extractor.compute(&stream)
    }
}

/// Live speaker labels from per-utterance embeddings.
pub struct EmbeddingTagger {
    embedder: SpeakerEmbedder,
    clusters: OnlineClusters,
}

impl EmbeddingTagger {
    pub fn open(models: &SpeakerModels, cfg: ClusterConfig) -> Result<Self> {
        Ok(Self {
            embedder: SpeakerEmbedder::open(models)?,
            clusters: OnlineClusters::new(cfg),
        })
    }
}

impl SpeakerTagger for EmbeddingTagger {
    fn identify(&mut self, samples: &[f32], sample_rate: u32) -> Option<String> {
        let emb = self.embedder.embed(samples, sample_rate)?;
        let ms = samples.len() as u64 * 1000 / sample_rate.max(1) as u64;
        self.clusters.assign(&emb, ms).map(label)
    }
}

/// Full offline diarization: pyannote segmentation, embeddings and
/// clustering over a whole recording. Built on the raw C API so it can
/// report progress, which the safe wrapper doesn't expose.
pub struct Diarizer {
    ptr: *const sys::OfflineSpeakerDiarization,
}

// SAFETY: sherpa-onnx diarizers are safe to use from one thread at a
// time; `process` takes `&self` but the C call doesn't mutate shared
// state beyond the object itself, as with the safe wrapper.
unsafe impl Send for Diarizer {}
unsafe impl Sync for Diarizer {}

type ProgressCallback = unsafe extern "C" fn(i32, i32, *mut c_void) -> i32;

extern "C" {
    // In the sherpa-onnx C API since 1.10; missing from sherpa-onnx-sys.
    fn SherpaOnnxOfflineSpeakerDiarizationProcessWithCallback(
        sd: *const sys::OfflineSpeakerDiarization,
        samples: *const f32,
        n: i32,
        callback: ProgressCallback,
        arg: *mut c_void,
    ) -> *const sys::OfflineSpeakerDiarizationResult;
}

/// Forwards sherpa's progress to the `&mut dyn FnMut` behind `arg`.
unsafe extern "C" fn progress_trampoline(done: i32, total: i32, arg: *mut c_void) -> i32 {
    // SAFETY: `arg` is the `&mut &mut dyn FnMut` passed by `process`,
    // alive for the duration of the call that invokes us.
    let f = unsafe { &mut *(arg as *mut &mut dyn FnMut(usize, usize)) };
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        f(done.max(0) as usize, total.max(0) as usize)
    }));
    0
}

impl Diarizer {
    /// `num_speakers` fixes the speaker count when known; otherwise the
    /// clustering threshold decides.
    pub fn open(models: &SpeakerModels, num_speakers: Option<usize>) -> Result<Self> {
        let cstr = |s: String| CString::new(s).map_err(|e| anyhow!("model path: {e}"));
        let seg = cstr(require(&models.segmentation, "speaker segmentation model")?)?;
        let emb = cstr(require(&models.embedding, "speaker embedding model")?)?;
        let cpu = cstr("cpu".into())?;
        // Defaults as in sherpa-onnx's own config.
        let config = sys::OfflineSpeakerDiarizationConfig {
            segmentation: sys::OfflineSpeakerSegmentationModelConfig {
                pyannote: sys::OfflineSpeakerSegmentationPyannoteModelConfig {
                    model: seg.as_ptr(),
                    window_shift_ratio: 0.1,
                },
                num_threads: models.num_threads,
                debug: 0,
                provider: cpu.as_ptr(),
            },
            embedding: sys::SpeakerEmbeddingExtractorConfig {
                model: emb.as_ptr(),
                num_threads: models.num_threads,
                debug: 0,
                provider: cpu.as_ptr(),
            },
            clustering: sys::FastClusteringConfig {
                num_clusters: num_speakers.map(|n| n as i32).unwrap_or(-1),
                threshold: 0.5,
                compute_confidence: 0,
            },
            min_duration_on: 0.3,
            min_duration_off: 0.5,
        };
        // SAFETY: `config` and the strings it points at outlive the call.
        let ptr = unsafe { sys::SherpaOnnxCreateOfflineSpeakerDiarization(&config) };
        if ptr.is_null() {
            return Err(anyhow!("sherpa-onnx failed to create the speaker diarizer"));
        }
        Ok(Self { ptr })
    }

    /// Speaker turns in `samples` (mono at `sample_rate`), in ms.
    pub fn process(&self, samples: &[f32], sample_rate: u32) -> Result<Vec<Segment>> {
        self.process_with_progress(samples, sample_rate, &mut |_, _| {})
    }

    /// Like [`Diarizer::process`], calling `progress(done, total)` as the
    /// speaker embeddings are computed (the slow part; it starts after
    /// segmentation, which reports nothing).
    pub fn process_with_progress(
        &self,
        samples: &[f32],
        sample_rate: u32,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<Vec<Segment>> {
        // SAFETY: valid diarizer pointer.
        let rate = unsafe { sys::SherpaOnnxOfflineSpeakerDiarizationGetSampleRate(self.ptr) };
        let audio = resample(samples, sample_rate, rate.max(1) as u32)?;
        let mut f: &mut dyn FnMut(usize, usize) = progress;
        // SAFETY: `audio` and `f` outlive the call; the trampoline casts
        // `arg` back to `&mut &mut dyn FnMut`.
        let result = unsafe {
            SherpaOnnxOfflineSpeakerDiarizationProcessWithCallback(
                self.ptr,
                audio.as_ptr(),
                audio.len() as i32,
                progress_trampoline,
                &mut f as *mut &mut dyn FnMut(usize, usize) as *mut c_void,
            )
        };
        if result.is_null() {
            return Err(anyhow!("speaker diarization failed"));
        }
        let ms = |s: f32| (s.max(0.0) * 1000.0).round() as u64;
        // SAFETY: `result` is non-null and destroyed once below; the
        // segment array has `n` entries and is freed after copying.
        let segments = unsafe {
            let n = sys::SherpaOnnxOfflineSpeakerDiarizationResultGetNumSegments(result).max(0);
            let mut out = Vec::with_capacity(n as usize);
            if n > 0 {
                let p = sys::SherpaOnnxOfflineSpeakerDiarizationResultSortByStartTime(result);
                if !p.is_null() {
                    out.extend(
                        std::slice::from_raw_parts(p, n as usize)
                            .iter()
                            .filter(|s| s.speaker >= 0)
                            .map(|s| Segment {
                                start_ms: ms(s.start),
                                end_ms: ms(s.end),
                                speaker: s.speaker as usize,
                            }),
                    );
                    sys::SherpaOnnxOfflineSpeakerDiarizationDestroySegment(p);
                }
            }
            sys::SherpaOnnxOfflineSpeakerDiarizationDestroyResult(result);
            out
        };
        Ok(segments)
    }
}

impl Drop for Diarizer {
    fn drop(&mut self) {
        // SAFETY: created by `open`, destroyed once.
        unsafe { sys::SherpaOnnxDestroyOfflineSpeakerDiarization(self.ptr) }
    }
}
