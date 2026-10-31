//! Working out what language a line is in, and putting a paragraph into
//! one language.
//!
//! The recognizer never says. Parakeet v3 decodes twenty-five languages
//! without being told which it is hearing, and what comes back is a
//! string: no language, no confidence, nothing to key a translation on.
//! So the language is read off the text afterwards, and the only thing
//! read off it here is the **script**.
//!
//! That is a real limit and it is deliberate. Russian against English is
//! Cyrillic against Latin, which is as unambiguous as text ever gets and
//! needs no model, no download and no network. German against English is
//! Latin against Latin, which this cannot tell apart at all and must not
//! pretend to -- a wrong guess sends a line to a model that translates it
//! from a language it is not in, and what comes back is confident
//! nonsense. [`Interpreter::new`] therefore refuses a pair that shares a
//! script, rather than guessing and being wrong later.
//!
//! Telling languages apart *within* a script is a different job wanting a
//! different tool (a classifier, or a recognizer that reports what it
//! heard). When there is one, it goes behind [`of`] and everything above
//! it keeps working.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{bail, Result};
use tracing::warn;

use crate::filters::sentences;
use crate::translate::Translate;

/// The alphabet a line is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Script {
    Latin,
    Cyrillic,
    /// Greek, Han, Hangul, Arabic, anything else: known to be none of
    /// the above, which is all a caller needs to leave it alone.
    Other,
}

/// How much of a line's letters have to agree before it is called a
/// script. Below this it is a line with a word of the other language in
/// it, which is not the same as a line in that language.
const MAJORITY: f32 = 0.7;

/// Letters a line needs before its script is worth believing. "OK" is
/// two letters of Latin in a Russian conversation, and the sentence
/// around it is the one that matters.
const ENOUGH: usize = 8;

/// The script `text` is written in, or `None` when there is too little
/// of it, or too much of a mixture, to say.
pub fn script(text: &str) -> Option<Script> {
    let mut latin = 0usize;
    let mut cyrillic = 0usize;
    let mut other = 0usize;
    for c in text.chars().filter(|c| c.is_alphabetic()) {
        match c {
            'a'..='z' | 'A'..='Z' => latin += 1,
            '\u{0400}'..='\u{04ff}' | '\u{0500}'..='\u{052f}' => cyrillic += 1,
            // Latin letters with diacritics, which are Latin: é, ü, ø.
            '\u{00c0}'..='\u{024f}' => latin += 1,
            _ => other += 1,
        }
    }
    let total = latin + cyrillic + other;
    if total < ENOUGH {
        return None;
    }
    let (best, n) = [
        (Script::Latin, latin),
        (Script::Cyrillic, cyrillic),
        (Script::Other, other),
    ]
    .into_iter()
    .max_by_key(|(_, n)| *n)?;
    (n as f32 / total as f32 >= MAJORITY).then_some(best)
}

/// The script a language is written in, for the languages this knows.
///
/// A language missing from here is one no [`Interpreter`] can be built
/// for, which is the honest answer: it would have nothing to decide on.
pub fn script_of(language: &str) -> Option<Script> {
    let l = language.trim();
    for (name, s) in [
        ("English", Script::Latin),
        ("German", Script::Latin),
        ("French", Script::Latin),
        ("Spanish", Script::Latin),
        ("Italian", Script::Latin),
        ("Portuguese", Script::Latin),
        ("Russian", Script::Cyrillic),
        ("Ukrainian", Script::Cyrillic),
        ("Bulgarian", Script::Cyrillic),
        ("Serbian", Script::Cyrillic),
        ("Greek", Script::Other),
        ("Chinese", Script::Other),
        ("Japanese", Script::Other),
        ("Korean", Script::Other),
    ] {
        if name.eq_ignore_ascii_case(l) {
            return Some(s);
        }
    }
    None
}

/// Which of `candidates` a line is in, by script alone. `None` when the
/// script cannot be read, or belongs to none of them, or to more than
/// one -- Russian and Ukrainian are both Cyrillic, and this has no way
/// of choosing between them.
pub fn of<'a>(text: &str, candidates: &'a [String]) -> Option<&'a str> {
    let s = script(text)?;
    let mut found = None;
    for c in candidates {
        if script_of(c) != Some(s) {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(c.as_str());
    }
    found
}

/// Whether a native language and the ones to translate are a set this
/// can decide between, with the reason when they are not.
///
/// Separate from [`Interpreter::new`] so a caller can say what is wrong
/// with its command line before it loads a translation model: the
/// languages are a mistake whatever translator was asked for, and
/// hearing about the model first only hides them.
pub fn pair(into: &str, from: &[String]) -> Result<()> {
    let Some(native) = script_of(into) else {
        bail!("{into} is not a language this knows the alphabet of");
    };
    if from.is_empty() {
        bail!("no language to translate from");
    }
    let mut seen = vec![native];
    for f in from {
        let Some(s) = script_of(f) else {
            bail!("{f} is not a language this knows the alphabet of");
        };
        if f.eq_ignore_ascii_case(into) {
            bail!("{f} is the language you read in, so there is nothing to translate");
        }
        if seen.contains(&s) {
            bail!(
                "{f} is written in the same alphabet as {}, and telling them apart \
                 is not something reading the alphabet can do",
                if s == native {
                    into.to_string()
                } else {
                    from.iter()
                        .filter(|o| script_of(o) == Some(s) && *o != f)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" / ")
                }
            );
        }
        seen.push(s);
    }
    Ok(())
}

/// What a paragraph's sentences have already been read as, by the
/// sentence as it was said.
///
/// This is what makes a translation worth making again before the
/// paragraph is finished: a paragraph that has grown by one sentence
/// costs one sentence to read, not the whole of it over. A sentence a
/// later pass rewrote is a different key and is translated again, which
/// is also the whole of the refinement -- there is nothing to invalidate
/// because the text *is* the key.
pub type Known = BTreeMap<String, String>;

/// One sentence in the native language, over the stretch of audio the
/// sentence it came from was spoken in.
///
/// The times are what makes translated subtitles possible: a translation
/// has its own word count and no timings of its own, but the sentence it
/// was made from has both, and a sentence is a small enough unit that
/// giving the translation its source's span is honest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// The sentence this was made from, as it was said. The key a
    /// caller keeps it under in [`Known`].
    pub source: String,
    pub text: String,
    /// Index of the first word of the source sentence in the
    /// paragraph's `words`, and one past its last.
    pub first_word: usize,
    pub last_word: usize,
}

/// A paragraph read in the native language: what it was heard in, the
/// whole of it, and its sentences for subtitling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    pub language: String,
    pub text: String,
    pub lines: Vec<Line>,
}

/// `text` up to the end of its last *finished* sentence, for
/// translating a paragraph somebody is still talking into.
///
/// What is cut off is the sentence being spoken now. A translator given
/// half a sentence returns half a thought, confidently, and it would be
/// replaced a moment later anyway -- so the reading runs a sentence
/// behind the words on screen, deliberately.
///
/// A terminator has to be followed by whitespace to count (see
/// [`crate::filters::sentence_end`]), which is what makes this work at
/// all: the last sentence of a paragraph nobody has added to yet has
/// nothing after it, so it is never mistaken for finished.
pub fn complete(text: &str) -> &str {
    let mut end = 0;
    let mut rest = text;
    while let Some(n) = crate::filters::sentence_end(rest) {
        end += n;
        rest = &rest[n..];
    }
    text[..end].trim_end()
}

/// Puts a paragraph into one language, a sentence at a time.
///
/// A sentence rather than a paragraph because that is the unit the
/// translators work in: a Marian model has no notion of context at all
/// and degrades on anything longer, and a chat model given a whole
/// paragraph is being asked to hold a structure it was not told to keep.
/// A sentence rather than an utterance because an utterance is where
/// somebody drew breath, which is not where a sentence ends -- passes 2
/// and 3 exist precisely to stitch those back together, and translating
/// per clip would take them apart again.
#[derive(Clone)]
pub struct Interpreter {
    /// The language to read in: what the operator speaks.
    into: String,
    /// The languages to look for and translate out of. Each is a
    /// different script from `into` and from the others, or
    /// [`Interpreter::new`] would have refused it.
    from: Vec<String>,
    by: Arc<dyn Translate>,
}

impl std::fmt::Debug for Interpreter {
    /// Named by what it does, not by its weights: a translation model
    /// has no useful printed form. Reads as `Russian into English, by
    /// local`, which is also what wants saying in a startup line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} into {}, by {}",
            self.from.join(" / "),
            self.into,
            self.by.name()
        )
    }
}

impl Interpreter {
    /// `into` is the native language, `from` the ones to translate; both
    /// as a language is spelled in English ("Russian").
    ///
    /// Refuses what it could not decide: an unknown language, and any
    /// pair sharing a script -- see the module comment for why that is a
    /// refusal and not a guess.
    pub fn new(into: &str, from: &[String], by: Arc<dyn Translate>) -> Result<Self> {
        pair(into, from)?;
        Ok(Self {
            into: into.to_string(),
            from: from.to_vec(),
            by,
        })
    }

    pub fn into_language(&self) -> &str {
        &self.into
    }

    /// The whole paragraph in the native language, or `None` when there
    /// is nothing to do: it is in the native language already -- the
    /// common case, and it costs nothing -- or there is too little of it,
    /// or too much of a mixture, for the alphabet to say which language
    /// it is in. Those are one answer here because they have one
    /// outcome: the line stands as it was said.
    ///
    /// `words` is the paragraph's word timings, in the same order as its
    /// text, for placing each sentence in time. `context` is the
    /// preceding paragraphs' readings, for a translator that can use
    /// them. `known` is what this paragraph's sentences have already
    /// been read as, so making the reading again as the paragraph grows
    /// costs only the sentences that are new.
    pub fn read(
        &self,
        text: &str,
        words: usize,
        context: &[String],
        known: &Known,
    ) -> Result<Option<Reading>> {
        let sentences = sentences(text);
        // Which language the paragraph as a whole is in decides the
        // direction, so one short sentence inside it cannot send a line
        // to the wrong model. The sentences are still translated one at
        // a time; this is only the choice of which way.
        let Some(language) = of(text, &self.from) else {
            return Ok(None);
        };
        let mut lines = Vec::with_capacity(sentences.len());
        let mut said: Vec<String> = context.to_vec();
        let mut at = 0usize;
        for s in sentences {
            let n = crate::filters::count_words(s);
            let (first, last) = (at.min(words), (at + n).min(words));
            at += n;
            // Already read, on a pass of this paragraph before this
            // one. The sentence is the key, so this is only a hit while
            // the sentence is still what it was.
            let line = match known.get(s) {
                Some(had) => had.clone(),
                // A sentence whose own script is the native one is
                // already read: a URL, a name, an English aside in a
                // Russian paragraph. Translating it would be a round
                // trip to get back what we started with, at best.
                None if script(s) == script_of(&self.into) => s.to_string(),
                None => self.by.translate(s, language, &self.into, &said)?,
            };
            said.push(line.clone());
            lines.push(Line {
                source: s.to_string(),
                text: line,
                first_word: first,
                last_word: last,
            });
        }
        if lines.is_empty() {
            return Ok(None);
        }
        let whole = lines
            .iter()
            .map(|l| l.text.trim())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if whole.trim().is_empty() {
            warn!(%language, "the translator returned nothing for {text:?}");
            bail!("the translation came back empty");
        }
        Ok(Some(Reading {
            language: language.to_string(),
            text: whole,
            lines,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A translator that marks what it was given rather than translating
    /// it, so a test can see which sentences reached it and in which
    /// direction.
    struct Marker;

    impl Translate for Marker {
        fn translate(
            &self,
            text: &str,
            from: &str,
            into: &str,
            _context: &[String],
        ) -> Result<String> {
            Ok(format!("<{from}->{into}: {text}>"))
        }

        fn name(&self) -> &'static str {
            "marker"
        }
    }

    fn interpreter() -> Interpreter {
        Interpreter::new("English", &["Russian".to_string()], Arc::new(Marker)).unwrap()
    }

    /// A translator that counts what it was asked to translate, so a
    /// test can see what a second pass over a growing paragraph costs.
    #[derive(Default)]
    struct Counter {
        asked: std::sync::Mutex<Vec<String>>,
    }

    impl Translate for Counter {
        fn translate(
            &self,
            text: &str,
            _from: &str,
            _into: &str,
            _context: &[String],
        ) -> Result<String> {
            self.asked.lock().expect("lock").push(text.to_string());
            Ok(format!("<{text}>"))
        }

        fn name(&self) -> &'static str {
            "counter"
        }
    }

    #[test]
    fn only_the_last_finished_sentence_is_offered_while_somebody_is_talking() {
        // The sentence being spoken now is cut off: it has no space
        // after its terminator because nothing follows it yet.
        assert_eq!(complete("Да, конечно. Я пью"), "Да, конечно.");
        assert_eq!(complete("Да, конечно. Я пью чай."), "Да, конечно.");
        // One more sentence lands and the one before it is finished.
        assert_eq!(
            complete("Да, конечно. Я пью чай. Вот"),
            "Да, конечно. Я пью чай."
        );
        // Nothing finished yet, so nothing to read.
        assert_eq!(complete("Да, конечно"), "");
        assert_eq!(complete(""), "");
    }

    #[test]
    fn a_paragraph_read_again_pays_only_for_what_is_new() {
        let by = Arc::new(Counter::default());
        let i = Interpreter::new("English", &["Russian".to_string()], by.clone()).unwrap();
        let mut known = Known::new();

        // First pass, while she is still talking: one finished sentence.
        let said = "Я пью чай на крыше дома. Вот";
        let r = i
            .read(complete(said), 6, &[], &known)
            .unwrap()
            .expect("a reading");
        for l in &r.lines {
            known.insert(l.source.clone(), l.text.clone());
        }
        assert_eq!(by.asked.lock().unwrap().len(), 1);

        // The paragraph has grown. The sentence already read is not
        // translated again; only the new one is.
        let said = "Я пью чай на крыше дома. Вот так, друзья мои. Ну";
        let r = i
            .read(complete(said), 11, &[], &known)
            .unwrap()
            .expect("a reading");
        assert_eq!(
            by.asked.lock().unwrap().as_slice(),
            ["Я пью чай на крыше дома.", "Вот так, друзья мои."]
        );
        assert_eq!(r.lines.len(), 2);
        assert_eq!(r.lines[0].text, "<Я пью чай на крыше дома.>");
        for l in &r.lines {
            known.insert(l.source.clone(), l.text.clone());
        }

        // Pass 3 rewrites the first sentence. It is a different
        // sentence, so it is a different key, so it is read again --
        // and the untouched one still isn't.
        let said = "Я пью чай на крыше своего дома. Вот так, друзья мои. Ну";
        i.read(complete(said), 12, &[], &known)
            .unwrap()
            .expect("a reading");
        assert_eq!(
            by.asked.lock().unwrap().as_slice(),
            [
                "Я пью чай на крыше дома.",
                "Вот так, друзья мои.",
                "Я пью чай на крыше своего дома."
            ]
        );
    }

    #[test]
    fn reads_the_script_a_line_is_written_in() {
        assert_eq!(script("Hello everyone, dear friends!"), Some(Script::Latin));
        assert_eq!(
            script("Здравствуйте, дорогие друзья!"),
            Some(Script::Cyrillic)
        );
        // Diacritics are Latin.
        assert_eq!(script("Grüße aus München, Freunde"), Some(Script::Latin));
        // Too little to tell, and too mixed to tell.
        assert_eq!(script("Да."), None);
        assert_eq!(script("Привет hello привет hello"), None);
    }

    #[test]
    fn a_language_is_only_named_when_the_script_names_it_alone() {
        let one = ["Russian".to_string()];
        assert_eq!(of("Я пью чай на крыше дома", &one), Some("Russian"));
        assert_eq!(of("I am drinking tea on the roof", &one), None);
        // Two Cyrillic candidates: the script cannot choose, so it
        // doesn't.
        let two = ["Russian".to_string(), "Ukrainian".to_string()];
        assert_eq!(of("Я пью чай на крыше дома", &two), None);
    }

    #[test]
    fn a_pair_that_shares_an_alphabet_is_refused_rather_than_guessed() {
        let by: Arc<dyn Translate> = Arc::new(Marker);
        // The whole point: this would otherwise send English prose to an
        // de->en model and get confident nonsense back.
        let e = Interpreter::new("English", &["German".to_string()], by.clone()).unwrap_err();
        assert!(format!("{e:#}").contains("same alphabet"), "{e:#}");
        // And two of each other's.
        let e = Interpreter::new(
            "English",
            &["Russian".to_string(), "Ukrainian".to_string()],
            by.clone(),
        )
        .unwrap_err();
        assert!(format!("{e:#}").contains("same alphabet"), "{e:#}");
        assert!(Interpreter::new("Klingon", &["Russian".to_string()], by).is_err());
    }

    #[test]
    fn a_pair_is_checked_without_a_translator_to_check_it_with() {
        // So a wrong command line is reported as a wrong command line,
        // before any weights are loaded to find it out with.
        assert!(pair("English", &["Russian".to_string()]).is_ok());
        for (into, from, says) in [
            ("English", vec!["German"], "same alphabet"),
            ("English", vec!["Russian", "Ukrainian"], "same alphabet"),
            ("English", vec!["English"], "nothing to translate"),
            ("English", vec![], "no language to translate from"),
            ("Klingon", vec!["Russian"], "knows the alphabet of"),
            ("English", vec!["Klingon"], "knows the alphabet of"),
        ] {
            let from: Vec<String> = from.into_iter().map(String::from).collect();
            let e = pair(into, &from).expect_err(&format!("{into} <- {from:?}"));
            assert!(format!("{e:#}").contains(says), "{e:#}");
        }
    }

    #[test]
    fn a_paragraph_already_in_the_native_language_is_left_alone() {
        let i = interpreter();
        let said = "Hello everyone, dear friends! It is twenty past six.";
        assert_eq!(i.read(said, 9, &[], &Known::new()).unwrap(), None);
    }

    #[test]
    fn each_sentence_goes_to_the_translator_on_its_own() {
        let i = interpreter();
        let said = "Да, конечно, дорогие друзья. Я пью чай на крыше дома.";
        let r = i
            .read(said, 10, &[], &Known::new())
            .unwrap()
            .expect("a reading");
        assert_eq!(r.language, "Russian");
        assert_eq!(
            r.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(),
            vec![
                "<Russian->English: Да, конечно, дорогие друзья.>",
                "<Russian->English: Я пью чай на крыше дома.>"
            ]
        );
        // Each sentence keeps the span of words it was made from, so a
        // subtitle can be cued from it.
        assert_eq!((r.lines[0].first_word, r.lines[0].last_word), (0, 4));
        assert_eq!((r.lines[1].first_word, r.lines[1].last_word), (4, 10));
        assert_eq!(
            r.text,
            r.lines
                .iter()
                .map(|l| l.text.clone())
                .collect::<Vec<_>>()
                .join(" ")
        );
    }

    #[test]
    fn an_aside_in_the_native_language_is_not_sent_round_the_trip() {
        let i = interpreter();
        // The paragraph is Russian, so the direction is settled; the
        // English sentence inside it is already readable as it stands.
        let said = "Я пью чай на крыше своего дома. The roof is flat. \
                    Мы живём в этой квартире уже давно.";
        let r = i
            .read(said, 18, &[], &Known::new())
            .unwrap()
            .expect("a reading");
        assert_eq!(r.lines[1].text, "The roof is flat.");
        assert!(r.lines[0].text.starts_with("<Russian->English:"));
        assert!(r.lines[2].text.starts_with("<Russian->English:"));
    }

    #[test]
    fn half_in_each_language_is_left_alone_rather_than_guessed_at() {
        let i = interpreter();
        // Not a paragraph in one language with an aside in the other:
        // a paragraph with no majority at all. There is no direction to
        // choose, and choosing one anyway is how a line gets translated
        // out of a language it was never in.
        let said = "Я пью чай на крыше. I am drinking tea on the roof.";
        assert_eq!(i.read(said, 11, &[], &Known::new()).unwrap(), None);
    }

    #[test]
    fn a_word_span_never_runs_off_the_end_of_the_timings() {
        let i = interpreter();
        // Fewer timings than words, which is what a paragraph whose
        // clips are still partial looks like.
        let r = i
            .read("Я пью чай на крыше дома. Вот так.", 3, &[], &Known::new())
            .unwrap()
            .expect("a reading");
        assert_eq!((r.lines[1].first_word, r.lines[1].last_word), (3, 3));
    }
}
