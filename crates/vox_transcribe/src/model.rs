//! Transcript data model. Times are milliseconds of *audio* since the
//! engine started (derived from the sample count, not the wall clock),
//! so a replayed file produces the same timeline as a live capture.

use serde::{Deserialize, Serialize};

/// Which pass last authored a clip's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    /// Pass 1: streaming partial, still growing (Zipformer, ALL CAPS).
    Partial,
    /// Pass 2: offline re-decode of the whole utterance (SenseVoice).
    Final,
    /// Pass 3: boundary re-transcription across neighbouring clips.
    Revised,
}

/// One VAD-closed utterance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub id: String,
    pub start_ms: u64,
    /// Unknown while the clip is still a streaming partial.
    pub duration_ms: Option<u64>,
    pub text: String,
    pub stage: Stage,
}

impl Clip {
    pub fn is_partial(&self) -> bool {
        self.stage == Stage::Partial
    }

    pub fn end_ms(&self) -> u64 {
        self.start_ms + self.duration_ms.unwrap_or(0)
    }
}

/// A run of clips separated by less than the paragraph gap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Paragraph {
    pub id: String,
    pub start_ms: u64,
    pub end_ms: u64,
    /// Space-join of the clips' texts.
    pub text: String,
    pub clips: Vec<Clip>,
    /// Soft/hard word cap reached: the next clip opens a new paragraph
    /// even without a silence gap.
    pub closed: bool,
    /// No further passes will touch this paragraph.
    pub hardened: bool,
    /// A pass-3 decode is running for this paragraph.
    pub pass3_inflight: bool,
}

impl Paragraph {
    pub(crate) fn new(id: String, first: Clip) -> Self {
        let mut p = Self {
            id,
            start_ms: first.start_ms,
            end_ms: first.start_ms,
            text: String::new(),
            clips: vec![first],
            closed: false,
            hardened: false,
            pass3_inflight: false,
        };
        p.rebuild();
        p
    }

    /// Recompute `text` and `end_ms` from the clip list.
    pub(crate) fn rebuild(&mut self) {
        self.text = self
            .clips
            .iter()
            .map(|c| c.text.as_str())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if let Some(first) = self.clips.first() {
            self.start_ms = first.start_ms;
        }
        if let Some(last) = self.clips.last() {
            self.end_ms = last.end_ms();
        }
    }

    /// True while any clip is still a streaming partial.
    pub fn has_partial(&self) -> bool {
        self.clips.iter().any(Clip::is_partial)
    }
}

/// Whole-session transcript.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Transcript {
    pub paragraphs: Vec<Paragraph>,
}

impl Transcript {
    /// Insert or replace a paragraph by id. Lets a consumer mirror the
    /// engine's state from [`crate::Event::Paragraph`] alone.
    pub fn upsert(&mut self, p: &Paragraph) {
        match self.paragraphs.iter_mut().find(|q| q.id == p.id) {
            Some(slot) => *slot = p.clone(),
            None => self.paragraphs.push(p.clone()),
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.paragraphs.retain(|p| p.id != id);
    }
}
