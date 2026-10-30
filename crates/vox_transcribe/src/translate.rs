//! Putting one line into another language.
//!
//! Two very different things can do this, so callers talk to the trait:
//!
//! * [`crate::openai`]'s translator asks a chat model. It follows
//!   instructions -- "copy a URL through character for character" -- and
//!   it can be shown the lines before this one, so a language that
//!   inflects agrees with what was already said. It costs a round trip,
//!   measured at about two seconds a sentence.
//! * [`crate::ct2`]'s asks a Marian model on the CPU, through
//!   CTranslate2. About forty times faster, no network at all, and no
//!   instructions to follow: it translates a shell command as prose,
//!   because to it that is what a shell command is.
//!
//! Which is wanted depends on what is being said, so neither is the
//! right default for everything -- see ROOM.md.

use anyhow::Result;

pub trait Translate: Send + Sync {
    /// `text` in `into`, as a language is spelled in English
    /// ("Russian"). `from` is the language it is already in: a chat
    /// model works that out for itself, but a Marian model is one
    /// direction only and has to be told.
    ///
    /// `context` is the lines before this one, oldest first, for a
    /// translator that can use them; one that cannot ignores them.
    fn translate(&self, text: &str, from: &str, into: &str, context: &[String]) -> Result<String>;

    /// Whether a URL, a path or a command has to be put in backticks to
    /// come back intact.
    ///
    /// True for a model that only knows to leave alone what is marked
    /// for it; false for one that can be told, in words, to copy
    /// anything that isn't prose through as it stands. Worth telling
    /// the people in the room, since it changes how they have to type.
    fn needs_backticks(&self) -> bool {
        true
    }

    /// For logs: which translator this is.
    fn name(&self) -> &'static str;
}

/// Text with every verbatim span taken out, and the spans that were
/// taken: what came out of [`protect`].
pub struct Protected {
    /// The line with each span replaced by `[n]`.
    pub masked: String,
    /// The spans, in the order their placeholders are numbered.
    pub spans: Vec<String>,
    /// The number the first placeholder was given.
    base: usize,
}

/// Take the backticked and fenced spans out of `text`, leaving `[1]`,
/// `[2]` … in their places, so a translator never sees them.
///
/// A model with no instructions to follow will happily render
/// `` `--language-also Russian` `` as the Russian for "also Russian
/// language", and `Ctrl-C` as "2-C". Keeping those out of its way is
/// the only way to get them back intact.
///
/// `[n]` is the placeholder because it is the one that survives. Marian
/// carries `[1]` and `{1}` through untouched in both directions, even
/// five of them in a line, but it corrupts `__1__` to `__1_` and drops
/// `⟦1⟧` altogether -- which would lose the span instead of mangling
/// it, and be worse. Numbering starts above any `[n]` already in the
/// line, so a line that talks about `[1]` itself still works.
pub fn protect(text: &str) -> Protected {
    let base = highest_placeholder(text) + 1;
    let mut masked = String::with_capacity(text.len());
    let mut spans = Vec::new();
    let mut rest = text;
    while let Some((before, span, after)) = next_span(rest) {
        masked.push_str(before);
        masked.push_str(&format!("[{}]", base + spans.len()));
        spans.push(span.to_string());
        rest = after;
    }
    masked.push_str(rest);
    Protected {
        masked,
        spans,
        base,
    }
}

impl Protected {
    /// Nothing was taken out, so the translator may have the line whole.
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// Put the spans back into a translated line.
    ///
    /// A placeholder the translator dropped would take its span with it,
    /// so anything missing is appended rather than lost: a URL at the
    /// end of the line is awkward, a URL nowhere is a bug.
    pub fn restore(&self, translated: &str) -> String {
        let mut out = translated.to_string();
        let mut missing = Vec::new();
        for (i, span) in self.spans.iter().enumerate() {
            let key = format!("[{}]", self.base + i);
            if out.contains(&key) {
                out = out.replace(&key, span);
            } else {
                missing.push(span.as_str());
            }
        }
        if !missing.is_empty() {
            out.push(' ');
            out.push_str(&missing.join(" "));
        }
        out
    }
}

/// The first fenced or backticked span: what is before it, the span
/// itself (its backticks included), and what is after.
fn next_span(text: &str) -> Option<(&str, &str, &str)> {
    // The longer fence first, or ``` would be read as an empty `` pair.
    for fence in ["```", "`"] {
        let Some(open) = text.find(fence) else {
            continue;
        };
        let after_open = open + fence.len();
        let Some(len) = text[after_open..].find(fence) else {
            continue;
        };
        let close = after_open + len + fence.len();
        return Some((&text[..open], &text[open..close], &text[close..]));
    }
    None
}

/// The biggest `n` in a `[n]` already in the text, or 0.
fn highest_placeholder(text: &str) -> usize {
    let mut best = 0;
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        rest = &rest[open + 1..];
        if let Some(close) = rest.find(']') {
            if let Ok(n) = rest[..close].parse::<usize>() {
                best = best.max(n);
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A translator that mangles every word, so what survives in the
    /// output is exactly what was kept out of its way.
    fn mangle(text: &str) -> String {
        text.split_whitespace()
            .map(|w| {
                if w.starts_with('[') {
                    w.to_string()
                } else {
                    "мусор".to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn a_command_comes_back_exactly_as_it_went_in() {
        let said = "Run `just room --language-also Russian` and press `Ctrl-C`.";
        let p = protect(said);
        assert_eq!(p.masked, "Run [1] and press [2].");
        assert_eq!(
            p.spans,
            vec!["`just room --language-also Russian`", "`Ctrl-C`"]
        );
        // Whatever the translator does to the prose, these two are not
        // its to touch.
        let out = p.restore(&mangle(&p.masked));
        assert!(out.contains("`just room --language-also Russian`"), "{out}");
        assert!(out.contains("`Ctrl-C`"), "{out}");
    }

    #[test]
    fn a_fence_is_one_span_not_two_empty_ones() {
        let p = protect("try ```cargo test -p vox_room``` first");
        assert_eq!(p.masked, "try [1] first");
        assert_eq!(p.spans, vec!["```cargo test -p vox_room```"]);
        assert_eq!(
            p.restore("сначала [1]"),
            "сначала ```cargo test -p vox_room```"
        );
    }

    #[test]
    fn a_line_with_nothing_to_protect_is_left_alone() {
        let p = protect("Yes, extremely hot.");
        assert!(p.is_empty());
        assert_eq!(p.masked, "Yes, extremely hot.");
        assert_eq!(p.restore("Да, очень жарко."), "Да, очень жарко.");
    }

    #[test]
    fn numbering_steps_over_a_placeholder_the_line_already_had() {
        // Otherwise restoring would rewrite the line's own "[1]".
        let p = protect("see note [1] and run `ls`");
        assert_eq!(p.masked, "see note [1] and run [2]");
        assert_eq!(p.restore("смотри [1] и [2]"), "смотри [1] и `ls`");
    }

    #[test]
    fn a_span_the_translator_dropped_is_appended_not_lost() {
        let p = protect("open `https://example.com/a_b` now");
        // The translation came back without the placeholder at all.
        let out = p.restore("открой сейчас");
        assert_eq!(out, "открой сейчас `https://example.com/a_b`");
    }

    #[test]
    fn an_unclosed_backtick_is_prose() {
        // Nothing to pair it with, so there is nothing to protect and
        // the line goes to the translator as it is.
        let p = protect("it costs 5` of pipe");
        assert!(p.is_empty());
        assert_eq!(p.masked, "it costs 5` of pipe");
    }
}
