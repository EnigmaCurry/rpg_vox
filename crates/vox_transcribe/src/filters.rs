//! Small text heuristics shared by every pass.

/// True when `text` ends on a sentence-terminating punctuation mark
/// (ASCII or CJK full-width).
pub fn ends_on_sentence(text: &str) -> bool {
    text.trim_end().chars().last().is_some_and(is_terminator)
}

/// Whitespace-separated word count.
pub fn count_words(text: &str) -> usize {
    text.split_whitespace().count()
}

/// SenseVoice hallucinations on near-silent or non-speech audio: the
/// lone "I." / "The." decodes, and tiny non-Latin fragments (one or two
/// CJK characters, a stray digit) with no whitespace.
pub fn is_false_positive(text: &str) -> bool {
    let trimmed = text.trim();
    if matches!(trimmed, "I." | "The.") {
        return true;
    }
    if trimmed.chars().any(char::is_whitespace) {
        return false;
    }
    if trimmed.chars().any(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    let content_chars = trimmed.chars().filter(|c| c.is_alphanumeric()).count();
    content_chars > 0 && content_chars <= 2
}

/// Pass-2 junk test: empty, punctuation-only, or a known false positive.
pub fn is_junk(text: &str) -> bool {
    let t = text.trim();
    t.is_empty() || !t.chars().any(|c| c.is_alphanumeric()) || is_false_positive(t)
}

/// True for sentence-terminating punctuation (ASCII or CJK full-width).
fn is_terminator(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | '。' | '！' | '？')
}

/// Byte length of the first whole sentence in `text`: up to a `.`, `?`,
/// `!` (and closing quotes) followed by whitespace, or a line break.
///
/// Used to speak an LLM's reply a sentence at a time as it streams, so
/// `None` means "wait for more text" -- which is why the terminator must
/// be followed by whitespace: "d.rymcg.tech" is one word, and a sentence
/// that ends the text so far may yet turn out to be an abbreviation.
///
/// Whitespace is also why this splits only languages that use it. The
/// full-width `。！？` that [`is_terminator`] knows are not matched here:
/// recognising them would need a rule that does not ask for a following
/// space, and no caller speaks a CJK language yet.
pub fn sentence_end(text: &str) -> Option<usize> {
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '\n' && !text[..i].trim().is_empty() {
            return Some(i + 1);
        }
        if matches!(c, '.' | '?' | '!') {
            let mut end = i + c.len_utf8();
            while let Some(&(j, q)) = chars.peek() {
                if matches!(q, '"' | '\'' | ')' | '”' | '’' | '.' | '?' | '!') {
                    end = j + q.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
            if text[end..].starts_with(char::is_whitespace) {
                return Some(end);
            }
        }
    }
    None
}

/// Words a clause needs before it is worth speaking on its own.
const CLAUSE_WORDS: usize = 4;

/// Byte length of the first clause in `text`: up to a `,`, `;`, `:` or
/// dash followed by whitespace, once there are a few words before it.
/// For the first thing said in a reply, where waiting for a whole
/// sentence is what the listener hears as lag.
pub fn clause_end(text: &str) -> Option<usize> {
    let mut words = 0;
    let mut in_word = false;
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            in_word = false;
            continue;
        }
        if !in_word {
            in_word = true;
            words += 1;
        }
        if matches!(c, ',' | ';' | ':' | '—' | '–') && words >= CLAUSE_WORDS {
            let end = i + c.len_utf8();
            if text[end..].starts_with(char::is_whitespace) {
                return Some(end);
            }
        }
    }
    None
}

/// Words whose trailing dot abbreviates rather than ends a sentence.
/// "Mr." and "St." are deliberately absent: what follows them is a name
/// and wants the capital anyway.
const ABBREVIATIONS: [&str; 5] = ["etc", "eg", "ie", "vs", "cf"];

/// Uppercase the first letter of `text`, skipping any opening quote or
/// bracket. The rest of the text is left exactly as it was.
pub fn capitalize_first(text: &str) -> String {
    let Some((i, c)) = text.char_indices().find(|(_, c)| c.is_alphanumeric()) else {
        return text.to_string();
    };
    if !c.is_lowercase() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..i]);
    out.extend(c.to_uppercase());
    out.push_str(&text[i + c.len_utf8()..]);
    out
}

/// Uppercase the first letter of every sentence but the first.
///
/// The recognizers punctuate text they do not re-case, so a pass-3
/// window decoded as one buffer comes back with a sentence break inside
/// it and no capital after it: "... in Hell. the surrounding land ...".
/// The text's own first word is left alone — a window, like a clip, can
/// begin mid-sentence.
pub fn capitalize_sentences(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        out.push(c);
        i += 1;
        if !is_terminator(c) {
            continue;
        }
        let run_start = i - 1;
        while i < chars.len() && is_terminator(chars[i]) {
            out.push(chars[i]);
            i += 1;
        }
        // "and then..." trails off; the next word continues the thought.
        let ellipsis = chars[run_start..i].iter().filter(|&&c| c == '.').count() >= 2;
        // A closing quote or bracket still belongs to the sentence ending.
        while i < chars.len() && matches!(chars[i], '"' | '\'' | '’' | '”' | ')' | ']') {
            out.push(chars[i]);
            i += 1;
        }
        // Only a mark followed by a space ends a sentence; the dots in
        // "3.5" and "e.g." do not.
        if i >= chars.len() || !chars[i].is_whitespace() {
            continue;
        }
        while i < chars.len() && chars[i].is_whitespace() {
            out.push(chars[i]);
            i += 1;
        }
        // An opening quote or bracket comes before the first letter.
        while i < chars.len() && matches!(chars[i], '"' | '\'' | '‘' | '“' | '(' | '[') {
            out.push(chars[i]);
            i += 1;
        }
        if ellipsis || is_abbreviation(&chars[..run_start]) {
            continue;
        }
        if let Some(&next) = chars.get(i) {
            if next.is_lowercase() {
                out.extend(next.to_uppercase());
                i += 1;
            }
        }
    }
    out
}

/// True when the word ending at the end of `before` is a known abbreviation.
fn is_abbreviation(before: &[char]) -> bool {
    let mut word: Vec<char> = before
        .iter()
        .rev()
        .take_while(|c| !c.is_whitespace())
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect();
    word.reverse();
    let word: String = word.into_iter().collect();
    ABBREVIATIONS.contains(&word.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn false_positives() {
        assert!(is_false_positive("I."));
        assert!(is_false_positive("嗯"));
        assert!(is_false_positive("1"));
        assert!(!is_false_positive("Yes."));
        assert!(!is_false_positive("I am."));
        assert!(is_junk("  ...  "));
        assert!(!is_junk("hello"));
    }

    #[test]
    fn capitalizes_sentence_starts() {
        assert_eq!(
            capitalize_sentences("in Hell. the surrounding land was red."),
            "in Hell. The surrounding land was red."
        );
        assert_eq!(
            capitalize_sentences("Wait! who said that?"),
            "Wait! Who said that?"
        );
        assert_eq!(
            capitalize_sentences("“stop.” he turned."),
            "“stop.” He turned."
        );
        // The first word is left alone: the text may continue a sentence.
        assert_eq!(
            capitalize_sentences("and then we left."),
            "and then we left."
        );
    }

    #[test]
    fn capitalizes_first_letter() {
        assert_eq!(capitalize_first("the road"), "The road");
        assert_eq!(capitalize_first("“stop.”"), "“Stop.”");
        assert_eq!(capitalize_first("The road"), "The road");
        assert_eq!(capitalize_first(""), "");
    }

    #[test]
    fn leaves_non_sentence_dots_alone() {
        for text in [
            "it cost 3.5 silver",
            "torches, rope, etc. then rations",
            "he paused... then spoke",
        ] {
            assert_eq!(capitalize_sentences(text), text);
        }
        assert_eq!(
            capitalize_sentences("Dr. ravenloft waited."),
            "Dr. Ravenloft waited."
        );
    }

    #[test]
    fn spots_a_finished_sentence() {
        assert!(ends_on_sentence("Done. "));
        assert!(ends_on_sentence("好。"));
        assert!(!ends_on_sentence("and then"));
    }

    #[test]
    fn finds_sentence_ends() {
        assert_eq!(sentence_end("Hello there. How"), Some(12));
        assert_eq!(sentence_end("Is it \"good?\" Yes"), Some(13));
        assert_eq!(sentence_end("Wait... what"), Some(7));
        assert_eq!(sentence_end("Line one\nnext"), Some(9));
        // A terminator nothing follows may yet be an abbreviation, and a
        // dotted name is one word: both wait for more text.
        assert_eq!(sentence_end("See d.rymcg.tech now"), None);
        assert_eq!(sentence_end("No end yet."), None);
    }

    #[test]
    fn finds_clause_ends() {
        assert_eq!(clause_end("Well, that is a fine question, and"), Some(30));
        // Too few words before the comma to be worth speaking alone.
        assert_eq!(clause_end("Well, sure"), None);
        assert_eq!(clause_end("It costs 1,000 coins, or"), Some(21));
    }
}
