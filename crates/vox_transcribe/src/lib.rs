//! Layered live speech-to-text, extracted from rpg_vox.
//!
//! * **Pass 1**: a streaming recognizer (Zipformer) shows words as they
//!   are spoken.
//! * **Pass 2**: when the VAD closes an utterance, an offline recognizer
//!   (SenseVoice) re-decodes it and replaces the partial.
//! * **Pass 3**: the last few clips of a paragraph are re-decoded as one
//!   buffer to fix words mangled at utterance boundaries.
//!
//! Feed mono `f32` samples into an [`Engine`] and consume [`Event`]s.

pub mod boundary;
pub mod engine;
pub mod filters;
pub mod model;
pub mod recognizer;
pub mod ring;
#[cfg(feature = "sherpa")]
pub mod sherpa;
#[cfg(unix)]
pub mod stderr;
pub mod vad;

pub use engine::{Change, Engine, EngineConfig, Event, ParagraphConfig, ParagraphMode, Pusher};
pub use model::{Clip, Paragraph, Stage, Transcript};
pub use recognizer::{OfflineRecognizer, StreamingRecognizer};
