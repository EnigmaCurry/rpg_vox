//! Transcript data model. Times are milliseconds of *audio* since the
//! engine started (derived from the sample count, not the wall clock),
//! so a replayed file produces the same timeline as a live capture.

use serde::{Deserialize, Serialize};

use crate::timing::{align_words, Word};

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
    /// One per word of `text`, from the offline pass that wrote it.
    /// Empty while the clip is a streaming partial.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<Word>,
    /// Speaker label ("A", "B", …) when diarization is on and the clip
    /// has been identified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
}

impl Clip {
    pub fn is_partial(&self) -> bool {
        self.stage == Stage::Partial
    }

    pub fn end_ms(&self) -> u64 {
        self.start_ms + self.duration_ms.unwrap_or(0)
    }

    /// `words` when they match `text`, else `text` spread over the clip.
    pub fn timed_words(&self) -> Vec<Word> {
        if self.words.len() == self.text.split_whitespace().count() {
            self.words.clone()
        } else {
            align_words(&self.words, &self.text, (self.start_ms, self.end_ms()))
        }
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
    /// One per word of `text` (finalized clips only), carried through
    /// pass 4's edits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<Word>,
    /// The first identified speaker among the clips. The engine starts a
    /// new paragraph when the speaker changes, so this covers them all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    /// Soft/hard word cap reached: the next clip opens a new paragraph
    /// even without a silence gap.
    pub closed: bool,
    /// No further passes will touch this paragraph.
    pub hardened: bool,
    /// A pass-3 decode is running for this paragraph.
    pub pass3_inflight: bool,
    /// Pass 4 (LLM correction) state; `None` when pass 4 is disabled or
    /// hasn't started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pass4: Option<Pass4>,
}

/// Pass-4 progress for one paragraph.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum Pass4 {
    Running,
    /// `original` is the pass-3 text the corrections were applied to;
    /// `text` on the paragraph holds the corrected version.
    Done {
        original: String,
        edits: usize,
    },
    /// The LLM call or its edits were rejected; `text` is unchanged.
    Failed {
        reason: String,
    },
}

impl Paragraph {
    pub(crate) fn new(id: String, first: Clip) -> Self {
        let mut p = Self {
            id,
            start_ms: first.start_ms,
            end_ms: first.start_ms,
            text: String::new(),
            clips: vec![first],
            words: Vec::new(),
            speaker: None,
            closed: false,
            hardened: false,
            pass3_inflight: false,
            pass4: None,
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
        self.words = self
            .clips
            .iter()
            .filter(|c| !c.is_partial())
            .flat_map(Clip::timed_words)
            .collect();
        self.speaker = self.clips.iter().find_map(|c| c.speaker.clone());
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
    /// engine's state from [`crate::Event::Paragraph`] alone. A new
    /// paragraph goes in time order, not at the end: a change of speaker
    /// can split one off an earlier paragraph after later ones exist.
    pub fn upsert(&mut self, p: &Paragraph) {
        match self.paragraphs.iter_mut().find(|q| q.id == p.id) {
            Some(slot) => *slot = p.clone(),
            None => {
                let at = self
                    .paragraphs
                    .iter()
                    .position(|q| q.start_ms > p.start_ms)
                    .unwrap_or(self.paragraphs.len());
                self.paragraphs.insert(at, p.clone());
            }
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.paragraphs.retain(|p| p.id != id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(id: &str, start_ms: u64) -> Paragraph {
        Paragraph::new(
            id.into(),
            Clip {
                id: format!("{id}c"),
                start_ms,
                duration_ms: Some(1000),
                text: id.into(),
                stage: Stage::Final,
                words: Vec::new(),
                speaker: None,
            },
        )
    }

    #[test]
    fn upsert_keeps_time_order() {
        let mut t = Transcript::default();
        t.upsert(&para("a", 30_000));
        t.upsert(&para("c", 41_000));
        // Split off "a" after "c" already exists.
        t.upsert(&para("b", 35_000));
        let ids: Vec<&str> = t.paragraphs.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
        // Replacing keeps the slot.
        t.upsert(&para("c", 41_000));
        assert_eq!(t.paragraphs.len(), 3);
    }
}
