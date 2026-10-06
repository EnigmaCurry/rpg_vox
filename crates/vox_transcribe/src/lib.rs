//! Layered live speech-to-text, extracted from rpg_vox.
//!
//! * **Pass 1**: a streaming recognizer (Zipformer) shows words as they
//!   are spoken.
//! * **Pass 2**: when the VAD closes an utterance, an offline recognizer
//!   (SenseVoice) re-decodes it and replaces the partial.
//! * **Pass 3**: the last few clips of a paragraph are re-decoded as one
//!   buffer to fix words mangled at utterance boundaries.
//!
//! * **Pass 4** (optional): once a paragraph settles, an LLM proposes
//!   small edits (number formats, misheard names, bogus sentence breaks).
//!
//! * **Speakers** (optional): each utterance is labelled live with a
//!   speaker; [`diarize`] relabels a finished transcript from a full
//!   offline diarization of the audio.
//!
//! Feed mono `f32` samples into an [`Engine`] and consume [`Event`]s.

pub mod boundary;
pub mod correct;
pub mod diarize;
pub mod engine;
pub mod filters;
pub mod gaps;
pub mod model;
#[cfg(feature = "llm")]
pub mod openai;
pub mod recognizer;
pub mod ring;
#[cfg(feature = "sherpa")]
pub mod sherpa;
pub mod speaker;
#[cfg(unix)]
pub mod stderr;
pub mod subtitle;
pub mod timing;
#[cfg(feature = "sherpa")]
pub mod tts;
pub mod vad;
pub mod vocab;

pub use engine::{Change, Engine, EngineConfig, Event, ParagraphConfig, ParagraphMode, Pusher};
pub use model::{Clip, Paragraph, Pass4, Stage, Transcript};
pub use recognizer::{OfflineRecognizer, SpeakerTagger, StreamingRecognizer, Transcription};
pub use timing::Word;
