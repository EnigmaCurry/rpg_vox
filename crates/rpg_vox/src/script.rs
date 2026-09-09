//! Speech-block extraction for the /script page.
//!
//! Assistant replies mix narrative markdown with `<speak>…</speak>` tags —
//! anything inside a tag is TTS material, everything else is out-of-character
//! description that only renders as markdown. This module walks the raw text
//! and produces the ordered list of speech-block payloads (trimmed) so the
//! store can persist one row per block.
//!
//! Nesting is not supported (LLM outputs don't nest), unclosed tags at the
//! tail are dropped, and case-insensitive matching keeps things forgiving.

/// Extract each `<speak>…</speak>` body from `text`, in document order.
/// Returns trimmed strings; empty bodies are skipped so an accidental
/// `<speak></speak>` doesn't produce a zero-length TTS take.
pub fn extract_speech_blocks(text: &str) -> Vec<String> {
    const OPEN: &str = "<speak>";
    const CLOSE: &str = "</speak>";

    let lower = text.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel_open) = lower[cursor..].find(OPEN) {
        let open_start = cursor + rel_open;
        let body_start = open_start + OPEN.len();
        let Some(rel_close) = lower[body_start..].find(CLOSE) else {
            break;
        };
        let body_end = body_start + rel_close;
        let body = text[body_start..body_end].trim();
        if !body.is_empty() {
            out.push(body.to_string());
        }
        cursor = body_end + CLOSE.len();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::extract_speech_blocks;

    #[test]
    fn extracts_single_block() {
        assert_eq!(
            extract_speech_blocks("intro <speak>hello</speak> outro"),
            vec!["hello".to_string()],
        );
    }

    #[test]
    fn extracts_multiple_blocks_in_order() {
        assert_eq!(
            extract_speech_blocks("a <speak>one</speak> b <speak>two</speak> c"),
            vec!["one".to_string(), "two".to_string()],
        );
    }

    #[test]
    fn trims_and_drops_empty_bodies() {
        assert_eq!(
            extract_speech_blocks("<speak>  hi  </speak><speak></speak><speak>bye</speak>"),
            vec!["hi".to_string(), "bye".to_string()],
        );
    }

    #[test]
    fn multiline_body_preserved() {
        let out = extract_speech_blocks("<speak>line one\nline two</speak>");
        assert_eq!(out, vec!["line one\nline two".to_string()]);
    }

    #[test]
    fn unclosed_tag_at_tail_dropped() {
        assert_eq!(
            extract_speech_blocks("<speak>okay</speak> and <speak>never ends"),
            vec!["okay".to_string()],
        );
    }

    #[test]
    fn no_blocks_returns_empty() {
        assert!(extract_speech_blocks("no tags here").is_empty());
    }

    #[test]
    fn case_insensitive_tags() {
        assert_eq!(
            extract_speech_blocks("<SPEAK>hi</Speak>"),
            vec!["hi".to_string()],
        );
    }
}
