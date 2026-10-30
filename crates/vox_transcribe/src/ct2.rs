//! Translation on the CPU, with no network: Marian models through
//! CTranslate2.
//!
//! One model per direction, which is the shape Marian comes in --
//! `opus-mt-en-ru` and `opus-mt-ru-en` are two different models, not one
//! model used two ways. That is why [`Translate::translate`] is told the
//! language to translate *from*: a chat model reads the line and works it
//! out, and this cannot.
//!
//! Laid out under the models directory by direction, in the codes the
//! model names use:
//!
//! ```text
//! <dir>/translate/en-ru/{model.bin,config.json,tokenizer.json,…}
//! <dir>/translate/ru-en/…
//! ```
//!
//! Each is loaded once and shared: `translate_batch` takes `&self`, so
//! every voice thread in the room reads the same 75 MB of weights.
//!
//! What it does not do is follow instructions. The chat translator is
//! told to copy a URL or a shell command through character for
//! character and does; this one has no such notion, and left to itself
//! renders `--language-also Russian` as the Russian for "also Russian
//! language" and `Ctrl-C` as "2-C".
//!
//! So it is kept away from them instead: [`crate::translate::protect`]
//! lifts every backticked and fenced span out of the line before the
//! model sees it. That makes anything marked up safe, and nothing else
//! -- which is why [`Translate::needs_backticks`] is true here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _, Result};
use ct2rs::tokenizers::auto::Tokenizer;
use ct2rs::{Config, TranslationOptions, Translator};
use tracing::info;

use crate::translate::{protect, Translate};

/// The directory under the models dir that holds the per-direction
/// model directories.
pub const TRANSLATE: &str = "translate";

/// The tokenizer the model is read with, and the reason it is this one
/// rather than the `source.spm` the opus-mt models ship.
///
/// CTranslate2 is handed token *strings* and maps them to ids itself
/// from the model's own `shared_vocabulary.json`, so a tokenizer only
/// has to split text into the same SentencePiece pieces. That can be
/// done by the pure-Rust `tokenizers` crate from a `tokenizer.json` --
/// and it has to be, because the SentencePiece library cannot be in
/// this binary: its protobuf and the one inside sherpa-onnx's static
/// libs are different versions of the same symbols. Statically that is
/// a linker error; with SentencePiece as a shared library it links and
/// then aborts at load, protobuf's own version check refusing 3.14
/// against 3.21.
///
/// `just download-translate-model-ru` writes this file beside the model
/// it downloads; see `tools/spm2tokenizer.py` for how, and for the
/// check that it splits text exactly as the .spm would.
pub const TOKENIZER: &str = "tokenizer.json";

/// The languages this knows a model code for, as a language is spelled
/// in English beside the code the opus-mt names use. Only pairs whose
/// directory is actually present are loaded, so this is a naming table
/// and not a claim about what is installed.
const CODES: [(&str, &str); 11] = [
    ("English", "en"),
    ("Russian", "ru"),
    ("German", "de"),
    ("French", "fr"),
    ("Spanish", "es"),
    ("Italian", "it"),
    ("Portuguese", "pt"),
    ("Chinese", "zh"),
    ("Japanese", "jap"),
    ("Korean", "ko"),
    ("Ukrainian", "uk"),
];

/// `language`'s model code, whatever case it was given in.
pub fn code(language: &str) -> Option<&'static str> {
    CODES
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(language.trim()))
        .map(|(_, code)| *code)
}

/// The directory a direction's model lives in: `translate/en-ru`.
pub fn direction_dir(dir: &Path, from: &str, into: &str) -> Option<PathBuf> {
    Some(
        dir.join(TRANSLATE)
            .join(format!("{}-{}", code(from)?, code(into)?)),
    )
}

/// Whether the model for one direction is downloaded.
pub fn has_direction(dir: &Path, from: &str, into: &str) -> bool {
    direction_dir(dir, from, into).is_some_and(|d| {
        ["model.bin", "config.json", TOKENIZER]
            .iter()
            .all(|f| d.join(f).is_file())
    })
}

pub struct Ct2Translator {
    /// By (from, into), as the languages are spelled in English.
    models: BTreeMap<(String, String), Translator<Tokenizer>>,
}

impl Ct2Translator {
    /// Load a model for every direction among `languages` that is
    /// downloaded. Directions that aren't are left out rather than
    /// refused: a room that only ever translates one way needs only the
    /// one model.
    pub fn load(dir: &Path, languages: &[String]) -> Result<Self> {
        let mut models = BTreeMap::new();
        for from in languages {
            for into in languages.iter().filter(|l| *l != from) {
                if !has_direction(dir, from, into) {
                    continue;
                }
                let d = direction_dir(dir, from, into).expect("checked just above");
                let t = Translator::new(&d, &Config::default()).with_context(|| {
                    format!("load the {from} to {into} model in {}", d.display())
                })?;
                info!(from = %from, into = %into, dir = %d.display(), "translating locally");
                models.insert((from.clone(), into.clone()), t);
            }
        }
        if models.is_empty() {
            bail!(
                "no translation models in {} (expected e.g. {}/en-ru; \
                 run `just download-translate-model-ru`)",
                dir.display(),
                dir.join(TRANSLATE).display()
            );
        }
        Ok(Self { models })
    }

    /// The directions loaded, for a startup line.
    pub fn directions(&self) -> Vec<String> {
        self.models
            .keys()
            .map(|(from, into)| format!("{from}-{into}"))
            .collect()
    }
}

impl Translate for Ct2Translator {
    /// `context` is ignored: a Marian model translates one sentence with
    /// no notion of what came before it, which is the other thing given
    /// up along with following instructions.
    fn translate(&self, text: &str, from: &str, into: &str, _context: &[String]) -> Result<String> {
        let t = self
            .models
            .get(&(from.to_string(), into.to_string()))
            .with_context(|| format!("no local model for {from} to {into}"))?;
        // Backticked and fenced spans never reach the model: it has no
        // way of being told to leave a command alone, so it is kept away
        // from them instead.
        let p = protect(text);
        let out = t
            .translate_batch(&[&p.masked], &TranslationOptions::default(), None)
            .with_context(|| format!("translate {from} to {into}"))?;
        let (said, _score) = out
            .into_iter()
            .next()
            .context("the translator returned nothing")?;
        Ok(p.restore(said.trim()).trim().to_string())
    }

    fn name(&self) -> &'static str {
        "local"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real models, through the real thing. Not run by default: it
    /// wants the pair downloaded.
    ///
    /// ```text
    /// VOX_SCRIBE_MODELS=… cargo test -p vox_transcribe --features ct2 \
    ///   -- --ignored --nocapture locally
    /// ```
    #[test]
    #[ignore = "loads the downloaded opus-mt models"]
    fn translates_locally_and_leaves_marked_up_text_alone() {
        let dir = std::env::var("VOX_SCRIBE_MODELS").expect("VOX_SCRIBE_MODELS");
        let dir = Path::new(&dir);
        let langs = ["English".to_string(), "Russian".to_string()];
        let t = Ct2Translator::load(dir, &langs).expect("load");
        println!("directions: {}", t.directions().join(", "));
        assert!(t.needs_backticks(), "or the backticks would be pointless");

        // Prose, both ways.
        for (text, from, into) in [
            ("Yes, extremely hot.", "English", "Russian"),
            ("Да, конечно.", "Russian", "English"),
        ] {
            let started = std::time::Instant::now();
            let got = t.translate(text, from, into, &[]).expect(text);
            println!(
                "  {from}->{into} {:>4} ms  {text:?}\n              -> {got:?}",
                started.elapsed().as_millis()
            );
            assert!(!got.is_empty());
            assert_ne!(got, text, "it should have translated something");
        }

        // And the thing the whole exercise is about: a command and a URL
        // come back character for character, because the model never saw
        // them.
        let said = "Run `just room --language-also Russian` then open \
                    `https://example.com/a_b-c?q=1&r=2`.";
        let got = t
            .translate(said, "English", "Russian", &[])
            .expect("marked up");
        println!("\n  {said:?}\n  -> {got:?}");
        assert!(
            got.contains("`just room --language-also Russian`"),
            "the command was mangled: {got}"
        );
        assert!(
            got.contains("`https://example.com/a_b-c?q=1&r=2`"),
            "the URL was mangled: {got}"
        );
        // The prose around them did get translated.
        assert!(!got.contains("Run "), "{got}");
    }
}

/// The two translators side by side, over the lines a room actually has
/// to carry. Not a test of anything: it prints, so the trade can be
/// looked at rather than remembered.
///
/// ```text
/// VOX_SCRIBE_MODELS=… VOX_SCRIBE_LLM_MODEL=… VOX_SCRIBE_LLM_KEY=… \
///   cargo test -p vox_transcribe --features ct2,llm --lib \
///   -- --ignored --nocapture compare
/// ```
#[cfg(all(test, feature = "llm"))]
mod compare {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::openai::{Endpoint, OpenAiConfig, OpenAiTranslator};

    /// Prose, then the things that are not prose: a URL, a command bare,
    /// and the same command in backticks.
    const LINES: [&str; 6] = [
        "A dragon is a legendary creature, often imagined as a giant reptile.",
        "Yes, extremely hot.",
        "Well, that depends -- do you mean the big one or the small one?",
        "Check https://example.com/a_b-c?q=1&r=2 before the meeting.",
        "Run just room --language-also Russian then press Ctrl-C.",
        "Run `just room --language-also Russian` then press `Ctrl-C`.",
    ];

    #[test]
    #[ignore = "asks both translators, so it wants the models and the endpoint"]
    fn compare_the_translators() {
        let dir = std::env::var("VOX_SCRIBE_MODELS").expect("VOX_SCRIBE_MODELS");
        let langs = ["English".to_string(), "Russian".to_string()];
        let local = Ct2Translator::load(Path::new(&dir), &langs).expect("local models");
        let ep = Endpoint::from_env();
        let remote = OpenAiTranslator::new(OpenAiConfig {
            base_url: ep.url.clone(),
            model: ep.model.clone().expect("VOX_SCRIBE_LLM_MODEL"),
            api_key: ep.api_key.clone(),
            vocabulary: Vec::new(),
            timeout: Duration::from_secs(30),
        });
        println!("local: on the CPU        remote: {:?}\n", ep.model);

        let mut totals = [Duration::ZERO; 2];
        for line in LINES {
            println!("  {line}");
            for t in [&local as &dyn Translate, &remote as &dyn Translate] {
                let started = Instant::now();
                let got = t.translate(line, "English", "Russian", &[]);
                let took = started.elapsed();
                totals[usize::from(t.name() == "openai")] += took;
                match got {
                    Ok(s) => println!("    {:<7} {:>5} ms  {s}", t.name(), took.as_millis()),
                    Err(e) => println!("    {:<7} failed: {e:#}", t.name()),
                }
            }
            println!();
        }
        let n = LINES.len() as u32;
        println!(
            "per line: local {} ms, openai {} ms",
            (totals[0] / n).as_millis(),
            (totals[1] / n).as_millis()
        );
    }
}
