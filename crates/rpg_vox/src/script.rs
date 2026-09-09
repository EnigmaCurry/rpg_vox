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

/// Which voice profile a script region gets synthesized in. Serialized as
/// a lowercase string to match the `role` column in `script_speech_blocks`.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Role {
    Narrator,
    Character,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Narrator => "narrator",
            Role::Character => "character",
        }
    }
}

/// Split an assistant reply into ordered speech regions covering the WHOLE
/// content — prose outside `<speak>` becomes a narrator region, each
/// `<speak>` body becomes a character region. Empty/whitespace-only
/// regions are dropped so we don't create zero-length TTS blocks between
/// adjacent tags.
///
/// This is the /script equivalent of "the entire response should be
/// narrated in two voices" — the narrator voice reads the surrounding
/// prose (stage directions, thoughts) and the character voice reads the
/// PC's spoken lines.
pub fn extract_script_regions(text: &str) -> Vec<(Role, String)> {
    const OPEN: &str = "<speak>";
    const CLOSE: &str = "</speak>";

    let lower = text.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel_open) = lower[cursor..].find(OPEN) {
        let open_start = cursor + rel_open;
        let body_start = open_start + OPEN.len();
        match lower[body_start..].find(CLOSE) {
            Some(rel_close) => {
                let body_end = body_start + rel_close;
                let prose = text[cursor..open_start].trim();
                if !prose.is_empty() {
                    out.push((Role::Narrator, prose.to_string()));
                }
                let speech = text[body_start..body_end].trim();
                if !speech.is_empty() {
                    out.push((Role::Character, speech.to_string()));
                }
                cursor = body_end + CLOSE.len();
            }
            None => {
                // Orphan `<speak>` — keep the prose that came BEFORE the
                // tag as a narrator region but drop everything from the
                // tag onward. We don't know where the speech was meant to
                // end, and the raw tag text shouldn't leak into any
                // remaining narrator block.
                let prose = text[cursor..open_start].trim();
                if !prose.is_empty() {
                    out.push((Role::Narrator, prose.to_string()));
                }
                return out;
            }
        }
    }
    let trailing = text[cursor..].trim();
    if !trailing.is_empty() {
        out.push((Role::Narrator, trailing.to_string()));
    }
    out
}

/// Character-only convenience wrapper — the legacy `<speak>` extractor,
/// kept because the tests still exercise it and callers that only care
/// about spoken lines don't need to filter the narrator regions out.
#[cfg(test)]
pub fn extract_speech_blocks(text: &str) -> Vec<String> {
    extract_script_regions(text)
        .into_iter()
        .filter_map(|(r, s)| (r == Role::Character).then_some(s))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Role, extract_script_regions, extract_speech_blocks};

    #[test]
    fn regions_alternate_narrator_character() {
        let out = extract_script_regions(
            "I draw my sword. <speak>Halt!</speak> I lower my stance. <speak>Who goes there?</speak>",
        );
        assert_eq!(
            out,
            vec![
                (Role::Narrator, "I draw my sword.".to_string()),
                (Role::Character, "Halt!".to_string()),
                (Role::Narrator, "I lower my stance.".to_string()),
                (Role::Character, "Who goes there?".to_string()),
            ],
        );
    }

    #[test]
    fn regions_pure_narration_yields_single_region() {
        let out = extract_script_regions("Silent. Watching. Waiting.");
        assert_eq!(
            out,
            vec![(Role::Narrator, "Silent. Watching. Waiting.".to_string())]
        );
    }

    #[test]
    fn regions_pure_speech_yields_single_region() {
        let out = extract_script_regions("<speak>Yes.</speak>");
        assert_eq!(out, vec![(Role::Character, "Yes.".to_string())]);
    }

    #[test]
    fn regions_empty_prose_between_tags_dropped() {
        let out = extract_script_regions("<speak>a</speak>   <speak>b</speak>");
        assert_eq!(
            out,
            vec![
                (Role::Character, "a".to_string()),
                (Role::Character, "b".to_string()),
            ]
        );
    }

    #[test]
    fn regions_unclosed_tag_stops_at_orphan() {
        // Legitimate prose before the orphan `<speak>` still comes through
        // as narrator; everything from the orphan onward (the raw tag text
        // and its uncloseable body) is dropped so we don't invent a
        // character block out of a truncated tag.
        let out = extract_script_regions("Good. <speak>done</speak> then <speak>never ends");
        assert_eq!(
            out,
            vec![
                (Role::Narrator, "Good.".to_string()),
                (Role::Character, "done".to_string()),
                (Role::Narrator, "then".to_string()),
            ]
        );
    }

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
