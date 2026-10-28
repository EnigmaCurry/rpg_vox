//! Speech-to-text: one layered `vox_transcribe` engine per person, all
//! sharing the same loaded models. Headsets make every mic a clean,
//! single-speaker source, so there is no diarization.

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
const ZIPFORMER: &str = "streaming-zipformer";
const PARAKEET: &str = "parakeet-tdt-0.6b-v2";

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
    streaming: Option<Zipformer>,
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
        let zf = dir.join(ZIPFORMER);
        let streaming = if present(
            &zf,
            &["encoder.onnx", "decoder.onnx", "joiner.onnx", "tokens.txt"],
        ) {
            Some(Zipformer::open(&ZipformerConfig::from_dir(&zf)).context("load Zipformer")?)
        } else {
            info!("no streaming model: text appears once each utterance is decoded");
            None
        };
        Ok(Self { offline, streaming })
    }

    /// A factory the room calls the first time each person's audio arrives.
    pub fn factory(self) -> EngineFactory {
        let models = Arc::new(self);
        Box::new(move |person: &str, room: Arc<Room>| models.engine(person, room))
    }

    fn engine(&self, person: &str, room: Arc<Room>) -> Result<Pusher> {
        let cfg = EngineConfig::new(RATE);
        let streaming = self
            .streaming
            .as_ref()
            .map(|z| Box::new(z.session()) as Box<dyn StreamingRecognizer>);
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
