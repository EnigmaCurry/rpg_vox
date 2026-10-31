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
//! * **Translation** (optional): a paragraph heard in another language
//!   is also read in the operator's own, a sentence at a time. See
//!   [`language`] for what decides which language it was heard in.
//!
//! * **Speakers** (optional): each utterance is labelled live with a
//!   speaker; [`diarize`] relabels a finished transcript from a full
//!   offline diarization of the audio.
//!
//! Feed mono `f32` samples into an [`Engine`] and consume [`Event`]s.

pub mod boundary;
pub mod correct;
#[cfg(feature = "ct2")]
pub mod ct2;
pub mod denoise;
pub mod diarize;
pub mod digits;
pub mod engine;
pub mod filters;
pub mod gaps;
pub mod language;
pub mod model;
#[cfg(feature = "llm")]
pub mod openai;
pub mod recognizer;
pub mod ring;
#[cfg(feature = "sherpa")]
pub mod sherpa;
pub mod speaker;
#[cfg(any(unix, windows))]
pub mod stderr;
pub mod subtitle;
pub mod timing;
pub mod translate;
#[cfg(feature = "sherpa")]
pub mod tts;
pub mod vad;
pub mod vocab;

pub use engine::{Change, Engine, EngineConfig, Event, ParagraphConfig, ParagraphMode, Pusher};
pub use model::{Clip, Paragraph, Pass4, Stage, Transcript, TranslatedLine, Translation};
pub use recognizer::{OfflineRecognizer, SpeakerTagger, StreamingRecognizer, Transcription};
pub use timing::Word;
