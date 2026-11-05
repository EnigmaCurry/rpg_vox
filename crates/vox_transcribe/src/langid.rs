//! Telling languages apart *within* one alphabet, off the text.
//!
//! [`crate::language::script`] decides Latin against Cyrillic for
//! nothing and is certain when it answers. What it cannot do is English
//! against French, or Russian against Ukrainian, and that is the whole
//! of what this is for: the question the alphabet could not answer, and
//! only that one.
//!
//! # Why it is asked restricted
//!
//! A language identifier will happily rank all 176 it knows. Asked that
//! way it is a harder problem than the one in front of us and fails in
//! ways that matter -- most of its confusions live among languages
//! nobody here is listening for, and a paragraph of Russian that comes
//! back Rusyn or Bulgarian is a wrong answer we had no reason to risk.
//!
//! So [`Classify::among`] is handed the candidates and nothing else. It
//! ranks over everything it knows, keeps the candidates' scores, and
//! renormalizes over those alone. Two consequences, both wanted: the
//! probability it returns is the probability *given* that the text is
//! one of them, which is what the caller actually knows to be true; and
//! a label outside the set cannot win, it can only fail to be evidence.
//!
//! By the time a caller gets here [`crate::language::of`] has already
//! cut the candidates to one alphabet, so the question is narrower
//! still: never "which of 25", usually "which of two".
//!
//! # What it is not allowed to do
//!
//! Be confidently wrong, cheaply. The cost of a wrong answer is not a
//! mislabelled line -- it picks the Marian direction, and a model told
//! to translate from a language the text is not in returns fluent
//! nonsense (see [`crate::ct2`]). That is why `among` returns a
//! probability rather than a label, and why
//! [`crate::language::CERTAIN`] exists to refuse below a floor: the
//! answer "I cannot tell" is already handled everywhere above here,
//! because it is the only answer the alphabet alone could give.

/// A classifier that can tell languages apart within one alphabet.
///
/// Always compiled, so [`crate::language::of`] has one signature
/// whatever the feature list; the implementations are what the
/// `langid-*` features turn on.
pub trait Classify: Send + Sync {
    /// What to call it in a log line.
    fn name(&self) -> &str;

    /// Which of `candidates` `text` is most likely in, as an index into
    /// `candidates`, and how sure -- renormalized over `candidates`
    /// alone, so it is a probability given that the text is one of
    /// them.
    ///
    /// `candidates` are language names as this codebase spells them
    /// ("Russian"), the same strings [`crate::language::script_of`]
    /// takes. `None` when none of them is a language the classifier
    /// knows, or when there is nothing in `text` to go on.
    fn among(&self, text: &str, candidates: &[&str]) -> Option<(usize, f32)>;
}

/// One line of `text`, which is what a line-oriented classifier can be
/// given: fastText reads a line at a time and treats a newline as the
/// end of one, so a paragraph handed over whole is classified on its
/// first line and the rest is silently dropped.
#[cfg(any(feature = "langid-fasttext", feature = "langid-lingua", test))]
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The index of the best candidate and its share of the candidates'
/// total, from `scores[i]` = the weight the classifier gave
/// `candidates[i]`, or `None` for a candidate it has no label for at
/// all.
///
/// A candidate the classifier cannot name takes the whole answer down
/// with it, rather than scoring zero. This is the trap: score it zero
/// and every *other* candidate's share goes up, so a classifier asked
/// to choose between one language it knows and three it has never heard
/// of answers the first one, at a share of 1.00, every time -- which
/// reads as certainty and is the opposite of it. There is no evidence
/// here about a language the model does not represent, and a renormalized
/// share is a claim that there is.
///
/// `None` too when every candidate scored nothing, for the same reason:
/// zero out of zero is not a tie.
#[cfg(any(feature = "langid-fasttext", feature = "langid-lingua", test))]
fn best(scores: &[Option<f32>]) -> Option<(usize, f32)> {
    let known: Vec<f32> = scores.iter().copied().collect::<Option<Vec<f32>>>()?;
    let total: f32 = known.iter().sum();
    if total <= 0.0 {
        return None;
    }
    let (i, &top) = known
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))?;
    Some((i, top / total))
}

#[cfg(feature = "langid-fasttext")]
pub use self::fasttext::{FastText, DIR, MODEL};

#[cfg(feature = "langid-fasttext")]
mod fasttext {
    use std::collections::BTreeMap;
    use std::path::Path;

    use anyhow::{Context as _, Result};
    use fasttext_pure_rs::FastText as Model;

    use super::{best, one_line, Classify};
    use crate::language::code_of;

    /// Where the model lives under a models directory:
    /// `<dir>/langid/lid.176.ftz`.
    ///
    /// A directory of its own rather than a file at the top, so that it
    /// is a model like the others -- one directory that can be
    /// installed, found and deleted whole, the way `translate/<pair>/`
    /// already is. A single file at the root of the models directory
    /// would make "delete this model" mean "delete that one file",
    /// which is a special case every caller would have to know about.
    pub const DIR: &str = "langid";

    /// The model file, as Facebook ships it.
    pub const MODEL: &str = "lid.176.ftz";

    /// fastText's `lid.176`, quantized: 917 KB for 176 languages.
    ///
    /// Read-only once loaded and `Send + Sync`, so one of these is
    /// shared by every thread that needs it -- the same arrangement
    /// [`crate::ct2`] uses for the translation weights, and for the
    /// same reason.
    pub struct FastText {
        model: Model,
    }

    impl FastText {
        /// Where the model is, under the models directory `dir`.
        pub fn path(dir: &Path) -> std::path::PathBuf {
            dir.join(DIR).join(MODEL)
        }

        /// Whether it is downloaded, for a caller that has to decide
        /// whether languages can be told apart before it loads
        /// anything.
        pub fn installed(dir: &Path) -> bool {
            Self::path(dir).is_file()
        }

        /// Load it from under the models directory `dir`.
        pub fn load(dir: &Path) -> Result<Self> {
            let path = Self::path(dir);
            let model = Model::load(path.to_string_lossy().as_ref())
                .with_context(|| format!("load the language classifier at {}", path.display()))?;
            Ok(Self { model })
        }
    }

    impl Classify for FastText {
        fn name(&self) -> &str {
            "fasttext lid.176"
        }

        fn among(&self, text: &str, candidates: &[&str]) -> Option<(usize, f32)> {
            let line = one_line(text);
            if line.is_empty() {
                return None;
            }
            // Every label, not the top few, and `lid.176` does return
            // all 176 for any input. Two reasons: a candidate that lost
            // outright still has to have its score looked up, since the
            // share returned is over the candidates; and a code that is
            // then still missing is a language the model has no label
            // for, which `best` has to be able to tell from a label
            // that merely scored nothing.
            let ranked = self.model.predict(&line, 176, 0.0).ok()?;
            let by_code: BTreeMap<&str, f32> = ranked
                .iter()
                .map(|p| (p.label.trim_start_matches("__label__"), p.probability))
                .collect();
            let scores: Vec<Option<f32>> = candidates
                .iter()
                .map(|c| code_of(c).and_then(|code| by_code.get(code).copied()))
                .collect();
            best(&scores)
        }
    }
}

#[cfg(feature = "langid-lingua")]
pub use self::lingua_backend::Lingua;

#[cfg(feature = "langid-lingua")]
mod lingua_backend {
    use std::collections::BTreeMap;

    use lingua::{Language, LanguageDetector, LanguageDetectorBuilder};

    use super::{best, one_line, Classify};
    use crate::language::code_of;

    /// lingua, over the languages its cargo features compiled in.
    ///
    /// Here to say what the 5 MB-per-language trade buys, not to be
    /// shipped: see the feature comment in `Cargo.toml`.
    pub struct Lingua {
        detector: LanguageDetector,
    }

    impl Default for Lingua {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Lingua {
        /// A detector over every language compiled in, built once.
        ///
        /// Over all of them rather than per candidate set so that one
        /// of these serves any call, and so the renormalizing is done
        /// the same way as the fastText backend's -- a detector built
        /// over two languages would normalize over two internally and
        /// the two backends would no longer be answering the same
        /// question.
        pub fn new() -> Self {
            Self {
                detector: LanguageDetectorBuilder::from_all_languages()
                    .with_preloaded_language_models()
                    .build(),
            }
        }
    }

    impl Classify for Lingua {
        fn name(&self) -> &str {
            "lingua"
        }

        fn among(&self, text: &str, candidates: &[&str]) -> Option<(usize, f32)> {
            let line = one_line(text);
            if line.is_empty() {
                return None;
            }
            // Keyed by ISO code rather than by name, because the two
            // sides spell languages differently and the codes are the
            // one thing `code_of`, fastText and lingua all agree on.
            let by_code: BTreeMap<String, f32> = self
                .detector
                .compute_language_confidence_values(&line)
                .into_iter()
                .map(|(l, v)| (iso(l), v as f32))
                .collect();
            // A language lingua was not compiled with is absent here,
            // and `best` refuses rather than letting the ones that were
            // compiled in win by its absence -- which is the whole of
            // why its languages being cargo features is a problem and
            // not a packaging detail.
            let scores: Vec<Option<f32>> = candidates
                .iter()
                .map(|c| code_of(c).and_then(|code| by_code.get(code).copied()))
                .collect();
            best(&scores)
        }
    }

    /// A lingua language as its ISO 639-1 code, lowercased, to meet
    /// `code_of` on the codes rather than on two spellings of a name.
    fn iso(l: Language) -> String {
        l.iso_code_639_1().to_string().to_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paragraph_is_flattened_to_the_one_line_a_classifier_reads() {
        // The bug this is for: a newline ends the line, so a paragraph
        // handed over whole would be classified on its first sentence
        // and judged on a fraction of the evidence.
        assert_eq!(
            one_line("Да, конечно.\nЯ пью чай."),
            "Да, конечно. Я пью чай."
        );
        assert_eq!(one_line("  spaced   out \t\n words "), "spaced out words");
        assert_eq!(one_line("   "), "");
    }

    #[test]
    fn the_share_returned_is_over_the_candidates_and_not_over_everything() {
        // What `among` has to do with a classifier's own numbers: these
        // are two candidates out of 176, and 0.6 against 0.2 is 75%
        // sure of the first, not 60%. Anything else would compare a
        // probability against a floor meant for a two-way decision.
        let (i, sure) = best(&[Some(0.6), Some(0.2)]).expect("evidence");
        assert_eq!(i, 0);
        assert!((sure - 0.75).abs() < 1e-6, "{sure}");
    }

    #[test]
    fn no_candidate_recognized_is_no_evidence_rather_than_a_tie() {
        // A classifier that knew none of the candidates scores them all
        // zero. Picking the first would be picking at random with a
        // confident-looking number attached to it.
        assert_eq!(best(&[Some(0.0), Some(0.0)]), None);
        assert_eq!(best(&[]), None);
    }

    /// The bench found this one: lingua compiled without Dutch called
    /// every Dutch sentence English, at a share of 1.00, because Dutch
    /// scored a zero it had no business scoring.
    #[test]
    fn a_candidate_the_classifier_cannot_name_is_not_a_zero() {
        // One known candidate against one it has no label for is not
        // 100% sure of the known one. It is no answer at all.
        assert_eq!(best(&[Some(0.99), None]), None);
        assert_eq!(best(&[None, Some(0.99)]), None);
        // Even where the known one would have won anyway, because the
        // share would be a claim about evidence that does not exist.
        assert_eq!(best(&[Some(0.4), Some(0.3), None]), None);
        // And a zero it *can* name is ordinary evidence.
        let (i, sure) = best(&[Some(0.4), Some(0.0)]).expect("evidence");
        assert_eq!((i, sure), (0, 1.0));
    }

    /// The real classifier on the pair the whole thing is for: English
    /// against French, which no alphabet can separate.
    ///
    /// Both directions, because only one of them is the obvious one. A
    /// French paragraph called French is the feature; an *English*
    /// paragraph called English is the safety, and it is the one that
    /// goes wrong quietly -- the language read in has to win on its own
    /// evidence, or every English paragraph is handed to an fr->en
    /// model. See `language::Interpreter::candidates`.
    ///
    /// An absolute path, because `cargo test` runs in the crate
    /// directory and a relative `models` would look for it there:
    ///
    /// ```text
    /// VOX_SCRIBE_MODELS=$PWD/models cargo test -p vox_transcribe \
    ///   --features langid-fasttext,sherpa-static --lib \
    ///   -- --ignored --nocapture apart
    /// ```
    #[cfg(feature = "langid-fasttext")]
    #[test]
    #[ignore = "loads the downloaded lid.176.ftz"]
    fn it_tells_english_and_french_apart() {
        let dir = std::env::var("VOX_SCRIBE_MODELS").expect("VOX_SCRIBE_MODELS");
        let by = FastText::load(std::path::Path::new(&dir)).expect("load the classifier");
        let candidates = ["English", "French"];
        // Paragraph-length, which is the unit `Interpreter::read` is
        // handed, and ordinary prose rather than anything chosen to be
        // easy.
        let cases = [
            (
                "English",
                "I am drinking tea on the roof of my house. This is the last month \
                 we live in this flat, and then we move. What I wanted to talk about \
                 today is a list I wrote of the things I mean to get done.",
            ),
            (
                "French",
                "Je bois du thé sur le toit de ma maison. C'est le dernier mois que \
                 nous habitons dans cet appartement, et ensuite nous déménageons. Ce \
                 dont je voulais parler aujourd'hui, c'est une liste que j'ai écrite.",
            ),
        ];
        for (want, text) in cases {
            let (i, sure) = by.among(text, &candidates).expect("an answer");
            println!("{want}: {} at {sure:.4}", candidates[i]);
            assert_eq!(candidates[i], want, "{text:?}");
            assert!(
                sure >= crate::language::CERTAIN,
                "{want} was only {sure} sure, under the floor of {}",
                crate::language::CERTAIN
            );
        }
        // And what it must refuse: a line with too little in it to be
        // either. `language::of` never gets this far -- `script` stops
        // short text before a classifier sees it -- but the classifier
        // should not be confident about it on its own account.
        let (_, sure) = by.among("OK.", &candidates).expect("an answer");
        assert!(sure < 1.0, "sure of three letters at {sure}");
    }
}
