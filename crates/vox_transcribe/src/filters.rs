//! Small text heuristics shared by every pass.

/// True when `text` ends on a sentence-terminating punctuation mark
/// (ASCII or CJK full-width).
pub fn ends_on_sentence(text: &str) -> bool {
    matches!(
        text.trim_end().chars().last(),
        Some('.' | '!' | '?' | '。' | '！' | '？'),
    )
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
    fn sentence_end() {
        assert!(ends_on_sentence("Done. "));
        assert!(ends_on_sentence("好。"));
        assert!(!ends_on_sentence("and then"));
    }
}
