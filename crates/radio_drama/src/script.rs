//! Reads a written radio play and returns the sequence of things that
//! happen in it.
//!
//! The format is the one a radio play gets written in when it is written
//! as markdown for people to read, which is the only format there is to
//! parse: there is no authoring tool here, and the script is expected to
//! stay readable on its own.
//!
//! ```text
//! ## SCENE TWO — THE VIRUS WITHOUT A MACHINE
//!
//! **INT. PRIVATE GROUND TRANSPORT — MOVING**
//!
//! [SFX: The slow, uncomfortable rattle of an Imperial civilian
//! conveyance.]
//!
//! **MAX:** Message from Voss.
//!
//! **VOSS** *(over vox)*: The message does not identify a location.
//!
//! [PAUSE.]
//! ```
//!
//! Four things follow from that shape and decide the whole parser:
//!
//! * **A paragraph is the unit, not a line.** Everything is hard-wrapped
//!   at about seventy columns, so a single cue or speech routinely spans
//!   three source lines. Each blank-line-separated paragraph is joined
//!   back into one string before anything looks at it.
//!
//! * **A speech continues until something else starts.** A speaker label
//!   is written once and the paragraphs after it belong to that speaker
//!   until a cue, a heading or another label intervenes. Those later
//!   paragraphs are deliberate beats — "The thirst." / "The screams." —
//!   so each becomes its own utterance, marked [`Line::continuation`] so
//!   the renderer can set them closer together than separate speeches.
//!
//! * **A direction belongs to the speech, not the paragraph.** `*(over
//!   vox)*` is written on the label and carries to every paragraph under
//!   it, because that is what it means.
//!
//! * **The script is longer than the play.** A front matter with a title,
//!   a dramatis personae table and a production convention note comes
//!   first, and production notes come after — and those notes contain
//!   `**Voss:**` paragraphs that are indistinguishable from dialogue by
//!   shape alone. So the body is bounded structurally: nothing is emitted
//!   until the first script element appears, and the first `#` heading
//!   *after* that point ends the play. Here that is `# EPISODE III …`
//!   being ignored (nothing has been emitted yet) and `# PRODUCTION
//!   NOTES` ending it. [`ParseOptions::end_at`] overrides this for a
//!   script the rule guesses wrong about.

use std::fmt;

use anyhow::{Context as _, Result};

/// One thing that happens in the play, in the order it happens.
#[derive(Debug, Clone, PartialEq)]
pub enum Element {
    /// A `##` heading: the start of a scene.
    Scene(String),
    /// A screenplay slug line — `**INT. ADMINISTRATUM — NIGHT**`. Not
    /// spoken: it tells a reader where they are, and a listener is told
    /// the same thing by the sound. Kept because it is the best label
    /// there is for the ambience a scene wants laid under it.
    Slug(String),
    /// Somebody says something.
    Line(Line),
    /// `[PAUSE.]` / `[LONG PAUSE.]` — a beat with nothing in it.
    Pause { long: bool },
    /// `[SFX: …]`, `[MUSIC: …]`, `[TRANSITION: …]` — an effect this
    /// version does not produce. Carried through to the manifest with a
    /// timestamp so a later pass has somewhere to put one.
    Cue { kind: String, text: String },
}

/// One utterance: the smallest thing handed to the synthesizer.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// The speaker as the script writes it, for reports and the manifest.
    pub speaker: String,
    /// The speaker put through [`normalize_speaker`], which is what the
    /// cast file is matched on.
    pub key: String,
    /// The parenthetical on the label, lowercased — `over vox`,
    /// `barely audible`. Carried to every paragraph of the speech.
    pub direction: Option<String>,
    /// What to say, with markdown emphasis removed.
    pub text: String,
    /// True when this is a later paragraph of a speech whose label was
    /// written above it, rather than the paragraph the label sits on.
    pub continuation: bool,
}

/// A parsed play.
#[derive(Debug, Clone)]
pub struct Script {
    /// The first `#` heading, if there is one.
    pub title: Option<String>,
    /// The first `###` heading, which is where this format puts the
    /// episode name.
    pub subtitle: Option<String>,
    pub elements: Vec<Element>,
}

#[derive(Debug, Clone, Default)]
pub struct ParseOptions {
    /// Stop at the first heading whose text contains this, compared
    /// case-insensitively, instead of at the first `#` after the body
    /// starts.
    pub end_at: Option<String>,
}

impl Script {
    pub fn parse_file(path: &std::path::Path, opts: &ParseOptions) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading script {}", path.display()))?;
        Ok(Self::parse(&text, opts))
    }

    pub fn parse(markdown: &str, opts: &ParseOptions) -> Self {
        Parser::new(opts).run(markdown)
    }

    pub fn lines(&self) -> impl Iterator<Item = &Line> {
        self.elements.iter().filter_map(|e| match e {
            Element::Line(l) => Some(l),
            _ => None,
        })
    }

    /// Every speaker in the play, in the order each is first heard, with
    /// how many utterances it has. First-heard order is what a cast file
    /// wants to be in: it puts the narrator at the top and the walk-on
    /// parts at the bottom.
    pub fn speakers(&self) -> Vec<SpeakerCount> {
        let mut out: Vec<SpeakerCount> = Vec::new();
        for line in self.lines() {
            match out.iter_mut().find(|s| s.key == line.key) {
                Some(found) => {
                    found.lines += 1;
                    found.words += line.text.split_whitespace().count();
                }
                None => out.push(SpeakerCount {
                    key: line.key.clone(),
                    display: line.speaker.clone(),
                    lines: 1,
                    words: line.text.split_whitespace().count(),
                }),
            }
        }
        out
    }
}

#[derive(Debug, Clone)]
pub struct SpeakerCount {
    pub key: String,
    pub display: String,
    pub lines: usize,
    pub words: usize,
}

impl fmt::Display for SpeakerCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({} lines)", self.display, self.lines)
    }
}

/// Fold a speaker label to the form the cast file is keyed on, so that
/// `**Voss:**` and `**VOSS:**` are one character, and `GM / NARRATOR`,
/// `GM/Narrator` and `gm / narrator` are too. Case goes, every kind of
/// dash becomes `-`, and the spaces around `/` and `-` go with it, since
/// whether a script writes `GM — TABLE` or `GM-TABLE` is not a fact
/// about who is speaking. The same function is applied to cast keys, so
/// the two always agree.
pub fn normalize_speaker(raw: &str) -> String {
    let mut s = String::with_capacity(raw.len());
    let mut pending_space = false;
    for ch in raw.trim().chars() {
        let ch = match ch {
            '\u{2014}' | '\u{2013}' | '\u{2012}' | '\u{2015}' | '\u{2212}' => '-',
            c => c,
        };
        if ch.is_whitespace() {
            pending_space = !s.is_empty();
            continue;
        }
        if pending_space && !matches!(ch, '/' | '-') && !s.ends_with(['/', '-']) {
            s.push(' ');
        }
        pending_space = false;
        s.extend(ch.to_uppercase());
    }
    s
}

/// Remove the markdown a reader sees through, so the synthesizer is not
/// handed asterisks to pronounce. Emphasis markers and code ticks go;
/// `[text](url)` keeps its text. Underscores are left alone: they are
/// more often part of a name here than an italic.
fn strip_inline_markdown(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' | '`' => i += 1,
            '[' => {
                // A link keeps its text and drops its target. Anything
                // else that opens a bracket is left as written; cues are
                // recognized before this ever runs.
                let close = chars[i..].iter().position(|&c| c == ']').map(|p| p + i);
                match close {
                    Some(close) if chars.get(close + 1) == Some(&'(') => {
                        let end = chars[close..]
                            .iter()
                            .position(|&c| c == ')')
                            .map(|p| p + close);
                        out.extend(&chars[i + 1..close]);
                        i = match end {
                            Some(end) => end + 1,
                            None => close + 1,
                        };
                    }
                    _ => {
                        out.push('[');
                        i += 1;
                    }
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What the parser is in the middle of when a paragraph arrives.
struct Parser<'a> {
    opts: &'a ParseOptions,
    elements: Vec<Element>,
    title: Option<String>,
    subtitle: Option<String>,
    /// The speech currently in progress, if any.
    active: Option<ActiveSpeaker>,
    /// Set once anything at all has been emitted, which is what makes a
    /// `#` heading mean "the play is over" rather than "the play is
    /// about to start".
    body_started: bool,
    done: bool,
}

struct ActiveSpeaker {
    display: String,
    key: String,
    direction: Option<String>,
    /// False until a paragraph has actually been spoken under this
    /// label. A label written on a line of its own — `**VOSS** *(with
    /// ritual gravity)*:` with the speech beginning in the paragraph
    /// below — must not mark that first paragraph as a continuation.
    spoke: bool,
}

impl<'a> Parser<'a> {
    fn new(opts: &'a ParseOptions) -> Self {
        Self {
            opts,
            elements: Vec::new(),
            title: None,
            subtitle: None,
            active: None,
            body_started: false,
            done: false,
        }
    }

    fn run(mut self, markdown: &str) -> Script {
        for para in paragraphs(markdown) {
            if self.done {
                break;
            }
            self.paragraph(&para);
        }
        Script {
            title: self.title,
            subtitle: self.subtitle,
            elements: self.elements,
        }
    }

    fn push(&mut self, el: Element) {
        self.body_started = true;
        self.elements.push(el);
    }

    fn paragraph(&mut self, para: &str) {
        let para = para.trim();
        if para.is_empty() {
            return;
        }
        if let Some(rest) = para.strip_prefix('#') {
            self.heading(para, rest);
            return;
        }
        // A table (the dramatis personae) and a horizontal rule are
        // layout, not performance. Both end whatever speech was running.
        if para.starts_with('|') || para.chars().all(|c| c == '-') && para.len() >= 3 {
            self.active = None;
            return;
        }
        if let Some(el) = parse_cue(para) {
            self.active = None;
            self.push(el);
            return;
        }
        if let Some((display, direction, rest)) = parse_speaker_label(para) {
            let key = normalize_speaker(&display);
            let text = strip_inline_markdown(rest);
            let spoke = !text.is_empty();
            if spoke {
                self.push(Element::Line(Line {
                    speaker: display.clone(),
                    key: key.clone(),
                    direction: direction.clone(),
                    text,
                    continuation: false,
                }));
            }
            self.active = Some(ActiveSpeaker {
                display,
                key,
                direction,
                spoke,
            });
            return;
        }
        if let Some(slug) = parse_slug(para) {
            self.active = None;
            self.push(Element::Slug(slug));
            return;
        }
        // Prose with no label on it. Inside a speech it is the next beat
        // of that speech; outside one it is the script talking to its
        // reader, and there is nothing to say.
        if let Some(active) = self.active.as_mut() {
            let text = strip_inline_markdown(para);
            if text.is_empty() {
                return;
            }
            let continuation = active.spoke;
            active.spoke = true;
            let line = Line {
                speaker: active.display.clone(),
                key: active.key.clone(),
                direction: active.direction.clone(),
                text,
                continuation,
            };
            self.push(Element::Line(line));
        }
    }

    fn heading(&mut self, whole: &str, after_first_hash: &str) {
        let level = 1 + after_first_hash.chars().take_while(|&c| c == '#').count();
        let text = whole.trim_start_matches('#').trim().to_string();
        self.active = None;

        if let Some(needle) = self.opts.end_at.as_deref() {
            if text.to_lowercase().contains(&needle.to_lowercase()) {
                self.done = true;
                return;
            }
        } else if level == 1 && self.body_started {
            // The play has been running and the script has gone back up
            // to a top-level heading: whatever follows is about the play
            // rather than in it.
            self.done = true;
            return;
        }

        match level {
            1 if self.title.is_none() => self.title = Some(text),
            3 if self.subtitle.is_none() => self.subtitle = Some(text),
            2 => self.push(Element::Scene(text)),
            _ => {}
        }
    }
}

/// Strip markdown blockquote markers from the front of a line.
///
/// A script quotes the words it wants set apart — Voss's prayers are
/// written this way — and they are still his lines. Without this the
/// `>` reaches the synthesizer, which is handed a character to
/// pronounce in place of the first word of a prayer.
fn strip_quote_marker(line: &str) -> &str {
    let mut rest = line;
    loop {
        match rest.strip_prefix('>') {
            Some(after) => rest = after.trim_start(),
            None => return rest,
        }
    }
}

/// Split on blank lines and unwrap each paragraph back into one string.
fn paragraphs(markdown: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for raw in markdown.lines() {
        let line = strip_quote_marker(raw.trim());
        if line.is_empty() {
            if !cur.is_empty() {
                out.push(cur.join(" "));
                cur.clear();
            }
        } else {
            cur.push(line);
        }
    }
    if !cur.is_empty() {
        out.push(cur.join(" "));
    }
    out
}

/// `[PAUSE.]`, `[LONG PAUSE.]`, `[SFX: …]`. Requires the whole paragraph
/// to be one bracketed span, which is what keeps the front matter's
/// "`[SFX]` denotes sound effects" sentence out of the play.
fn parse_cue(para: &str) -> Option<Element> {
    let inner = para.strip_prefix('[')?.strip_suffix(']')?;
    if inner.contains(']') {
        return None;
    }
    let flat = inner.trim().trim_end_matches('.').trim();
    let upper = flat.to_uppercase();
    if upper == "PAUSE" {
        return Some(Element::Pause { long: false });
    }
    if upper == "LONG PAUSE" || upper == "BEAT" {
        return Some(Element::Pause {
            long: upper != "BEAT",
        });
    }
    let (kind, text) = match inner.split_once(':') {
        Some((kind, text)) => (kind.trim().to_uppercase(), text.trim().to_string()),
        // A bracketed span with no colon is still a production note of
        // some kind; keep it under its own text as the kind so nothing
        // written in the script goes missing from the manifest.
        None => (flat.to_uppercase(), String::new()),
    };
    Some(Element::Cue {
        kind,
        text: strip_inline_markdown(&text),
    })
}

/// Both ways this format writes a speaker:
///
/// * `**MAX:** Message from Voss.`
/// * `**VOSS** *(over vox)*: The message does not identify a location.`
///
/// Returns the label as written, the parenthetical lowercased, and
/// whatever follows the colon — which is legitimately empty when the
/// speech starts in the next paragraph.
fn parse_speaker_label(para: &str) -> Option<(String, Option<String>, &str)> {
    let rest = para.strip_prefix("**")?;
    let (label, after) = rest.split_once("**")?;
    let label = label.trim();
    if label.is_empty() {
        return None;
    }

    // `**MAX:** …` — the colon is inside the bold.
    if let Some(name) = label.strip_suffix(':') {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        return Some((name.to_string(), None, after.trim_start()));
    }

    // `**VOSS** *(over vox)*: …` — a direction, then the colon.
    let after = after.trim_start();
    let (direction, after) = match after.strip_prefix("*(") {
        Some(tail) => {
            let (inside, tail) = tail.split_once(")*")?;
            (Some(inside.trim().to_lowercase()), tail.trim_start())
        }
        None => (None, after),
    };
    let after = after.strip_prefix(':')?;
    Some((label.to_string(), direction, after.trim_start()))
}

/// `**INT. ADMINISTRATUM — NIGHT**`: a paragraph that is entirely bold
/// and carries no colon, so it cannot be mistaken for a label.
fn parse_slug(para: &str) -> Option<String> {
    let inner = para.strip_prefix("**")?.strip_suffix("**")?;
    if inner.contains("**") || inner.contains(':') || inner.trim().is_empty() {
        return None;
    }
    Some(inner.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(md: &str) -> Script {
        Script::parse(md, &ParseOptions::default())
    }

    fn lines(md: &str) -> Vec<Line> {
        parse(md).lines().cloned().collect()
    }

    #[test]
    fn normalizes_away_case_dashes_and_spacing() {
        assert_eq!(normalize_speaker("Voss"), "VOSS");
        assert_eq!(normalize_speaker("GM / NARRATOR"), "GM/NARRATOR");
        assert_eq!(normalize_speaker("gm/narrator"), "GM/NARRATOR");
        assert_eq!(normalize_speaker("GM \u{2014} TABLE"), "GM-TABLE");
        assert_eq!(normalize_speaker("GM-TABLE"), "GM-TABLE");
        assert_eq!(normalize_speaker("  head   librarian "), "HEAD LIBRARIAN");
    }

    #[test]
    fn unwraps_a_hard_wrapped_speech() {
        let got = lines(
            "**GM / NARRATOR:** There are few things in the Imperium more\ndangerous than an unanswered question.\n",
        );
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].text,
            "There are few things in the Imperium more dangerous than an unanswered question."
        );
    }

    #[test]
    fn later_paragraphs_continue_the_speech() {
        let got = lines("**GM / NARRATOR:** The thirst.\n\nThe screams.\n\nThey need more.\n");
        assert_eq!(got.len(), 3);
        assert!(got.iter().all(|l| l.key == "GM/NARRATOR"));
        assert_eq!(
            got.iter().map(|l| l.continuation).collect::<Vec<_>>(),
            vec![false, true, true]
        );
    }

    #[test]
    fn a_direction_carries_to_the_whole_speech() {
        let got = lines(
            "**VOSS** *(over vox)*: Analysis continues.\n\nI shall remain on warm standby.\n",
        );
        assert_eq!(got.len(), 2);
        assert!(got
            .iter()
            .all(|l| l.direction.as_deref() == Some("over vox")));
        assert_eq!(got[0].text, "Analysis continues.");
    }

    #[test]
    fn a_label_alone_does_not_make_the_next_paragraph_a_continuation() {
        let got = lines("**VOSS** *(with ritual gravity)*:\n\nFrom the weakness of the mind.\n\nOmnissiah save us.\n");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].text, "From the weakness of the mind.");
        assert!(!got[0].continuation);
        assert!(got[1].continuation);
    }

    /// A quoted prayer is still the line of whoever is speaking, and
    /// the `>` is not part of what they say.
    #[test]
    fn a_blockquote_is_spoken_without_its_marker() {
        let got = lines(
            "**VOSS** *(with ritual gravity)*:\n\n> By the Motive Force, authenticate.\n> Omnissiah, bless these ledgers.\n",
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].key, "VOSS");
        assert_eq!(got[0].direction.as_deref(), Some("with ritual gravity"));
        assert_eq!(
            got[0].text,
            "By the Motive Force, authenticate. Omnissiah, bless these ledgers."
        );
        assert!(!got[0].continuation);
    }

    #[test]
    fn a_nested_quote_marker_goes_too() {
        let got = lines("**MAX:** He wrote:\n\n>> Nobody came.\n");
        assert_eq!(got[1].text, "Nobody came.");
    }

    #[test]
    fn a_cue_inside_a_blockquote_is_still_a_cue() {
        let got = parse("**MAX:** Listen.\n\n> [SFX: A door closes.]\n");
        assert_eq!(
            got.elements[1],
            Element::Cue {
                kind: "SFX".into(),
                text: "A door closes.".into(),
            }
        );
    }

    #[test]
    fn a_cue_ends_the_speech_running_into_it() {
        let got =
            parse("**MAX:** Message from Voss.\n\n[SFX: Data slate activates.]\n\nStill Max?\n");
        assert_eq!(
            got.elements,
            vec![
                Element::Line(Line {
                    speaker: "MAX".into(),
                    key: "MAX".into(),
                    direction: None,
                    text: "Message from Voss.".into(),
                    continuation: false,
                }),
                Element::Cue {
                    kind: "SFX".into(),
                    text: "Data slate activates.".into(),
                },
            ]
        );
    }

    #[test]
    fn recognizes_pauses_and_multi_line_cues() {
        let got = parse(
            "**MAX:** What?\n\n[PAUSE.]\n\n[LONG PAUSE.]\n\n[SFX: Ecclesiastical bells. Traffic.\nDistant crowds.]\n",
        );
        assert_eq!(got.elements[1], Element::Pause { long: false });
        assert_eq!(got.elements[2], Element::Pause { long: true });
        assert_eq!(
            got.elements[3],
            Element::Cue {
                kind: "SFX".into(),
                text: "Ecclesiastical bells. Traffic. Distant crowds.".into(),
            }
        );
    }

    #[test]
    fn keeps_scenes_and_slugs_and_spells_neither_aloud() {
        let got = parse("## SCENE ONE \u{2014} THE SIXFOLD MEMORY\n\n**INT. TRANSIT TRAIN \u{2014} MOVING**\n\n**MAX:** Hello.\n");
        assert_eq!(
            got.elements[0],
            Element::Scene("SCENE ONE \u{2014} THE SIXFOLD MEMORY".into())
        );
        assert_eq!(
            got.elements[1],
            Element::Slug("INT. TRANSIT TRAIN \u{2014} MOVING".into())
        );
        assert_eq!(got.lines().count(), 1);
    }

    #[test]
    fn front_matter_and_production_notes_are_not_in_the_play() {
        let md = "\
# HARSH OFFWORLD JUSTICE

### Episode III \u{2014} The Pleasure of Being Deceived

A radio dramatization.

| Voice | Direction |
| ----- | --------- |
| VOSS  | Precise   |

Production convention: `[SFX]` denotes sound effects.

# EPISODE III \u{2014} THE PLEASURE OF BEING DECEIVED

## PROLOGUE

**VOSS:** Our approaches are complementary.

# PRODUCTION NOTES

## Character and performance direction

**Voss:** His prayers should be played completely straight.
";
        let got = parse(md);
        assert_eq!(got.title.as_deref(), Some("HARSH OFFWORLD JUSTICE"));
        assert_eq!(
            got.subtitle.as_deref(),
            Some("Episode III \u{2014} The Pleasure of Being Deceived")
        );
        let said: Vec<&str> = got.lines().map(|l| l.text.as_str()).collect();
        assert_eq!(said, vec!["Our approaches are complementary."]);
    }

    #[test]
    fn end_at_overrides_the_structural_rule() {
        let md = "## ONE\n\n**MAX:** Hello.\n\n## APPENDIX\n\n**MAX:** Goodbye.\n";
        let opts = ParseOptions {
            end_at: Some("appendix".into()),
        };
        let got = Script::parse(md, &opts);
        assert_eq!(got.lines().count(), 1);
    }

    #[test]
    fn strips_emphasis_from_what_is_spoken() {
        let got = lines("**MAX:** The title, *The Pleasure of Being Deceived*, is a `joke`.\n");
        assert_eq!(
            got[0].text,
            "The title, The Pleasure of Being Deceived, is a joke."
        );
    }

    #[test]
    fn counts_speakers_in_the_order_they_are_first_heard() {
        let got = parse("**MAX:** One.\n\n**VOSS:** Two.\n\n**Max:** Three.\n");
        let speakers = got.speakers();
        assert_eq!(
            speakers.iter().map(|s| s.key.as_str()).collect::<Vec<_>>(),
            vec!["MAX", "VOSS"]
        );
        assert_eq!(speakers[0].lines, 2);
    }
}
