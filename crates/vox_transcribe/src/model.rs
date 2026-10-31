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
    /// The reading in the native language, for a paragraph heard in
    /// another one. `None` when translation is off or hasn't started;
    /// [`Translation::Native`] when it ran and there was nothing to do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translation: Option<Translation>,
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

/// The native-language reading of one paragraph.
///
/// It never replaces [`Paragraph::text`]. What was said is the record --
/// it is what the word timings, the karaoke subtitles and any later pass
/// are about, and when a translation looks wrong it is the only way to
/// see whether the translator or the recognizer was at fault.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum Translation {
    /// Being read for the first time: there is nothing to show yet.
    /// Only ever the first one -- a paragraph being read again as it
    /// grows keeps the reading it has until the new one lands, because
    /// taking it off the screen to put it back is worse than it being a
    /// sentence out of date.
    Running,
    /// It was heard in `language` and reads as `text`. `lines` is one
    /// entry per sentence, with the span of audio its source was spoken
    /// in, for subtitles.
    Done {
        /// The paragraph text this was made from. A reading is made
        /// before the paragraph is finished, so it can be behind
        /// [`Paragraph::text`]; when this is not equal to it, what is
        /// on screen is a sentence or two short and another reading is
        /// coming. A paragraph never hardens in that state.
        of: String,
        language: String,
        text: String,
        lines: Vec<TranslatedLine>,
    },
    /// Nothing to put into another language: it is written in the
    /// alphabet you read, or there is too little of it to say
    /// otherwise. Recorded rather than left as `None` so the pass is
    /// not asked for it twice -- `of` is what it was decided for, and
    /// more words can change the answer.
    Native { of: String },
    /// The translator failed on the text in `of`; what was said is all
    /// there is. Tried once more when the paragraph settles, since by
    /// then there is nothing to lose by waiting for it.
    Failed { of: String, reason: String },
}

/// One sentence of a translation, over the stretch of audio the sentence
/// it came from was spoken in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslatedLine {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

impl Translation {
    /// The reading, for a paragraph that has one.
    pub fn text(&self) -> Option<&str> {
        match self {
            Translation::Done { text, .. } => Some(text),
            _ => None,
        }
    }

    /// The language it was heard in, for a paragraph that was translated.
    pub fn language(&self) -> Option<&str> {
        match self {
            Translation::Done { language, .. } => Some(language),
            _ => None,
        }
    }

    /// The paragraph text this was decided for, or `None` while the
    /// first reading is still being made.
    pub fn of(&self) -> Option<&str> {
        match self {
            Translation::Running => None,
            Translation::Done { of, .. }
            | Translation::Native { of }
            | Translation::Failed { of, .. } => Some(of),
        }
    }

    /// Whether this is the reading of `text` and not of some earlier,
    /// shorter version of the paragraph.
    pub fn is_of(&self, text: &str) -> bool {
        self.of() == Some(text)
    }
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
            translation: None,
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

    /// The whole transcript as plain text: the non-empty paragraphs, a
    /// blank line between them.
    pub fn text(&self) -> String {
        self.paragraphs
            .iter()
            .map(|p| p.text.trim())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
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

    #[test]
    fn text_skips_the_empty_paragraphs() {
        let mut t = Transcript::default();
        t.upsert(&para("a", 0));
        let mut blank = para("blank", 1_000);
        blank.text = "  ".into();
        t.upsert(&blank);
        t.upsert(&para("b", 2_000));
        assert_eq!(t.text(), "a\n\nb");
        assert_eq!(Transcript::default().text(), "");
    }
}
