//! Speech-to-text: one layered `vox_transcribe` engine per person, all
//! sharing the same loaded models. Headsets make every mic a clean,
//! single-speaker source, so there is no diarization.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context as _, Result};
use tracing::info;
use vox_transcribe::sherpa::{
    Parakeet, ParakeetConfig, SenseVoice, SenseVoiceConfig, Zipformer, ZipformerConfig,
};
use vox_transcribe::{Engine, EngineConfig, Event, OfflineRecognizer, Pusher, StreamingRecognizer};

use crate::mixer::RATE;
use crate::room::{EngineFactory, Room};

// Directory names inside the models dir, as `scribe` downloads them.
const SENSE_VOICE: &str = "sense-voice";
const PARAKEET: &str = "parakeet-tdt-0.6b-v3";

/// The streaming (pass-1) model for each language, by the name the room
/// gives that language. Pass 1 is the only pass that needs one per
/// language: Parakeet, which does passes 2 and 3, detects the language
/// from the audio. A language with no model here still gets its words
/// written down, once each utterance is decoded -- only the text that
/// appears while someone is still talking is missing.
const STREAMING: [(&str, &str); 2] = [
    ("English", "streaming-zipformer"),
    ("Russian", "streaming-zipformer-ru"),
];

/// The same per-user models directory `scribe` downloads into, so a
/// machine that has run scribe has the models already.
pub fn default_models_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("VOX_SCRIBE_MODELS") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/vox_scribe/models")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("vox_scribe/models")
    }
}

pub struct Models {
    offline: Arc<dyn OfflineRecognizer>,
    /// Pass 1 by language, for those whose model is downloaded. Each is
    /// loaded once and shared: a session per person comes off it.
    streaming: BTreeMap<String, Zipformer>,
}

fn present(dir: &Path, files: &[&str]) -> bool {
    files.iter().all(|f| dir.join(f).is_file())
}

impl Models {
    /// Load Parakeet (else SenseVoice) for the final pass and Zipformer,
    /// if present, for the instant streaming text.
    pub fn load(dir: &Path) -> Result<Self> {
        let pk = dir.join(PARAKEET);
        let sv = dir.join(SENSE_VOICE);
        let offline: Arc<dyn OfflineRecognizer> = if present(
            &pk,
            &[
                "encoder.int8.onnx",
                "decoder.int8.onnx",
                "joiner.int8.onnx",
                "tokens.txt",
            ],
        ) {
            Arc::new(Parakeet::open(&ParakeetConfig::from_dir(&pk)).context("load Parakeet")?)
        } else if present(&sv, &["model.int8.onnx", "tokens.txt"]) {
            Arc::new(SenseVoice::open(&SenseVoiceConfig::from_dir(&sv)).context("load SenseVoice")?)
        } else {
            bail!(
                    "no speech-to-text model in {} (run `scribe` once to download them, or pass --models-dir)",
                    dir.display()
                );
        };
        let mut streaming = BTreeMap::new();
        for (language, bundle) in STREAMING {
            let zf = dir.join(bundle);
            if !present(
                &zf,
                &["encoder.onnx", "decoder.onnx", "joiner.onnx", "tokens.txt"],
            ) {
                continue;
            }
            let model = Zipformer::open(&ZipformerConfig::from_dir(&zf))
                .with_context(|| format!("load the {language} Zipformer"))?;
            streaming.insert(language.to_string(), model);
        }
        if streaming.is_empty() {
            info!("no streaming model: text appears once each utterance is decoded");
        } else {
            info!(
                languages = %streaming.keys().cloned().collect::<Vec<_>>().join(", "),
                "streaming text"
            );
        }
        Ok(Self { offline, streaming })
    }

    /// A factory the room calls the first time each person's audio
    /// arrives, with the language they speak.
    pub fn factory(self) -> EngineFactory {
        let models = Arc::new(self);
        Box::new(move |person: &str, language: &str, room: Arc<Room>| {
            models.engine(person, language, room)
        })
    }

    fn engine(&self, person: &str, language: &str, room: Arc<Room>) -> Result<Pusher> {
        let cfg = EngineConfig::new(RATE);
        // Found whatever case the language was spelled in: a room started
        // with `english` in its environment must still find the model
        // filed under `English`, rather than quietly go without.
        let model = self
            .streaming
            .iter()
            .find(|(l, _)| l.eq_ignore_ascii_case(language))
            .map(|(_, z)| z);
        // No model for their language: they still get the decoded text,
        // just not while they are still speaking. Worth saying out loud,
        // because the difference is visible to everyone in the room.
        if model.is_none() {
            info!(
                person,
                language, "no streaming model for this language: text appears a phrase behind"
            );
        }
        let streaming = model.map(|z| Box::new(z.session()) as Box<dyn StreamingRecognizer>);
        let engine = Engine::spawn(cfg, streaming, self.offline.clone());
        let pusher = engine.pusher();
        let events = engine.events().clone();
        let person = person.to_string();
        std::thread::Builder::new()
            .name(format!("vox-room-stt-{person}"))
            .spawn(move || {
                // Keep the engine alive for as long as its events are read.
                let _engine = engine;
                for ev in events {
                    match ev {
                        Event::Paragraph { paragraph, .. } => {
                            room.on_paragraph(&person, &paragraph)
                        }
                        Event::ParagraphRemoved { id } => room.on_paragraph_removed(&id),
                        Event::Level { speaking, .. } => room.on_level(&person, speaking),
                    }
                }
            })
            .context("spawn transcription thread")?;
        info!("transcribing");
        Ok(pusher)
    }
}
