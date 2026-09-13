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
    OnlineRecognizer, OnlineRecognizerConfig, OnlineStream,
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

// ---------------------------------------------------------------------------
// Streaming (online) recognizer — used by the Record page's live transcript
// so text appears word-by-word instead of after silence closes each turn.
// ---------------------------------------------------------------------------

/// Paths + tuning for a streaming Zipformer transducer. Three ONNX files
/// (encoder/decoder/joiner) plus the shared `tokens.txt`. Any of the
/// standard sherpa-onnx streaming Zipformer bundles works; the
/// English-only 20M variant is small and fast enough for real-time on
/// modest CPUs, the bilingual/multilingual variants trade size for
/// broader language coverage.
#[derive(Clone, Debug)]
pub struct StreamingSttConfig {
    pub encoder: Option<PathBuf>,
    pub decoder: Option<PathBuf>,
    pub joiner: Option<PathBuf>,
    pub tokens: Option<PathBuf>,
    pub num_threads: i32,
}

impl Default for StreamingSttConfig {
    fn default() -> Self {
        Self {
            encoder: None,
            decoder: None,
            joiner: None,
            tokens: None,
            num_threads: 2,
        }
    }
}

/// Cloneable handle to a loaded streaming recognizer. The wrapped
/// `OnlineRecognizer` is `Send + Sync` per sherpa-onnx's own
/// annotations, so multiple slot workers can hand it stream references
/// concurrently.
#[derive(Clone)]
pub struct StreamingSttHandle {
    inner: Arc<OnlineRecognizer>,
}

impl StreamingSttHandle {
    /// Load the streaming recognizer from disk. `Ok(None)` when *all*
    /// paths are unset (streaming STT explicitly disabled — the
    /// Record page falls back to offline SenseVoice for finals in
    /// that case, matching pre-streaming behaviour). Missing files
    /// with any path set is a hard error to surface typos loudly at
    /// startup.
    pub fn open(cfg: &StreamingSttConfig) -> Result<Option<Self>> {
        let any_set = cfg.encoder.is_some()
            || cfg.decoder.is_some()
            || cfg.joiner.is_some()
            || cfg.tokens.is_some();
        if !any_set {
            info!("streaming STT disabled (no --streaming-stt-* paths configured)");
            return Ok(None);
        }
        let encoder = cfg
            .encoder
            .as_ref()
            .context("streaming STT encoder path required")?;
        let decoder = cfg
            .decoder
            .as_ref()
            .context("streaming STT decoder path required")?;
        let joiner = cfg
            .joiner
            .as_ref()
            .context("streaming STT joiner path required")?;
        let tokens = cfg
            .tokens
            .as_ref()
            .context("streaming STT tokens path required")?;
        for (label, p) in [
            ("encoder", encoder),
            ("decoder", decoder),
            ("joiner", joiner),
            ("tokens", tokens),
        ] {
            if !p.is_file() {
                return Err(anyhow!(
                    "streaming STT {label} file not found: {}",
                    p.display()
                ));
            }
        }

        let mut config = OnlineRecognizerConfig::default();
        config.model_config.transducer.encoder =
            Some(encoder.to_string_lossy().into_owned());
        config.model_config.transducer.decoder =
            Some(decoder.to_string_lossy().into_owned());
        config.model_config.transducer.joiner =
            Some(joiner.to_string_lossy().into_owned());
        config.model_config.tokens = Some(tokens.to_string_lossy().into_owned());
        config.model_config.num_threads = cfg.num_threads;
        config.decoding_method = Some("greedy_search".into());
        // Sherpa's built-in endpointer. Our VAD still owns the outer
        // "is the user speaking" decision (it feeds this recognizer
        // and drives the recording's audio timeline), so these
        // thresholds are only a safety net for when the VAD's
        // silence-end threshold hasn't fired yet but the model
        // already sees a long trailing pause. Values match the
        // sherpa demo defaults.
        config.enable_endpoint = true;
        config.rule1_min_trailing_silence = 2.4;
        config.rule2_min_trailing_silence = 1.2;
        config.rule3_min_utterance_length = 20.0;

        let recognizer = OnlineRecognizer::create(&config).ok_or_else(|| {
            anyhow!("sherpa-onnx failed to create streaming recognizer (bad model paths?)")
        })?;
        info!(
            encoder = %encoder.display(),
            threads = cfg.num_threads,
            "streaming STT recognizer loaded (Zipformer transducer)"
        );
        Ok(Some(Self { inner: Arc::new(recognizer) }))
    }

    /// Open a fresh per-utterance session. Each Vox slot's VAD worker
    /// owns one session for its lifetime and calls
    /// [`StreamingSession::reset`] between utterances — the underlying
    /// sherpa `OnlineStream` is stateful, so sessions should not be
    /// shared across slots.
    pub fn new_session(&self) -> StreamingSession {
        let stream = self.inner.create_stream();
        StreamingSession {
            recognizer: self.inner.clone(),
            stream,
        }
    }
}

/// Per-worker streaming decode session. Holds the recognizer's stream
/// state and a back-reference to the shared recognizer for its
/// decode/reset/is_endpoint calls.
///
/// All operations are synchronous and call into blocking C code —
/// callers are expected to run this on a dedicated thread (e.g. via
/// `tokio::task::spawn_blocking`) rather than on an async runtime
/// worker, otherwise per-chunk decode will stall other tasks.
pub struct StreamingSession {
    recognizer: Arc<OnlineRecognizer>,
    stream: OnlineStream,
}

impl StreamingSession {
    /// Feed one chunk of mono f32 PCM at `sample_rate`, then run
    /// as many decode steps as the recognizer says it has audio for.
    /// After this call, [`Self::current_text`] reflects the most
    /// recent partial hypothesis.
    pub fn feed(&self, samples: &[f32], sample_rate: u32) {
        if samples.is_empty() {
            return;
        }
        self.stream.accept_waveform(sample_rate as i32, samples);
        while self.recognizer.is_ready(&self.stream) {
            self.recognizer.decode(&self.stream);
        }
    }

    /// Current partial hypothesis. Trimmed. Empty string when the
    /// recognizer has nothing yet (e.g. only silence fed so far).
    pub fn current_text(&self) -> String {
        self.recognizer
            .get_result(&self.stream)
            .map(|r| r.text.trim().to_string())
            .unwrap_or_default()
    }

    /// True when sherpa's endpointer thinks the current utterance has
    /// ended (long trailing silence per the configured rules). VAD
    /// finalization typically fires first, so this is mostly a fallback.
    #[allow(dead_code)]
    pub fn is_endpoint(&self) -> bool {
        self.recognizer.is_endpoint(&self.stream)
    }

    /// Drop the current utterance's decode state so the next Start
    /// begins clean. Called both when a too-short utterance is
    /// discarded and at normal utterance-close — we do not use the
    /// streaming decoder's final text (the caller re-transcribes with
    /// the offline SenseVoice model for higher accuracy), so a full
    /// flush would just waste CPU.
    pub fn reset(&self) {
        self.recognizer.reset(&self.stream);
    }
}

/// Same non-fatal loader idiom as [`open_or_warn`] but for the streaming
/// recognizer. When streaming is unavailable the Record page falls back
/// to the offline SenseVoice path.
pub fn open_streaming_or_warn(cfg: &StreamingSttConfig) -> Option<StreamingSttHandle> {
    match StreamingSttHandle::open(cfg) {
        Ok(handle) => handle,
        Err(err) => {
            warn!(
                err = %format!("{err:#}"),
                "streaming STT disabled — failed to load recognizer"
            );
            None
        }
    }
}
