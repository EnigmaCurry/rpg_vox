//! A conversation as a file somebody can take away.
//!
//! One renderer, with bounds: a transcript of a whole conversation and
//! a transcript of the stretch a recording covers are the same document
//! with a different slice of entries in it. Writing them twice is how
//! two transcripts of the same afternoon come to disagree about
//! translations, pictures and the gaps.
//!
//! What it will not do is pretend. A conversation holds pictures,
//! recordings and whole plays as well as words, and a text file cannot
//! hold any of those -- so each one gets a line saying it was there and
//! what it was, because a transcript that drops them lies about the
//! afternoon by leaving them out. The off-the-record notes come through
//! as the ordinary entries they are, for the same reason they exist: a
//! gap nobody explains is indistinguishable from an hour in which
//! nobody spoke.

use crate::room::{Entry, Kind, Seat};

/// What the file is a transcript of.
pub enum What {
    /// Everything a conversation holds.
    Conversation,
    /// The stretch of one that a recording covers, and how long the
    /// clip beside it runs for.
    Recording { secs: f32 },
}

/// Everything the document says about itself.
pub struct Head<'a> {
    /// The room it was said in, by the name people call it.
    pub room: &'a str,
    /// The conversation's number, which is what names the file and
    /// what the room's own history lists it by.
    pub conversation: i64,
    /// What somebody called it; empty is unnamed.
    pub title: &'a str,
    /// Unix milliseconds the conversation was started.
    pub at: u64,
    /// The language to read it in. A line with a translation into it is
    /// given in it, with the original underneath -- which is what the
    /// screen already does, and for the same reason: a translation that
    /// went wrong can be seen to have.
    pub language: &'a str,
    /// Minutes east of UTC: the clock to spell the times in.
    ///
    /// The room never decides this. A date is a thing you spell in the
    /// reader's own clock, so the browser asking for the file says
    /// which one that is and the file says in its head what it used --
    /// a transcript with bare times and no zone on it is a transcript
    /// nobody can line up against anything else. Zero is UTC, which is
    /// what the room itself falls back to when there is no reader to
    /// ask (a recording ends on its own account).
    pub clock: i32,
    pub what: What,
}

/// The whole document.
pub fn markdown(head: &Head, entries: &[Entry]) -> String {
    let mut out = String::new();
    let (date, time) = stamp(head.at, head.clock);
    let name = if head.title.is_empty() {
        format!("{} — {date} {time}", head.room)
    } else {
        head.title.to_string()
    };
    out.push_str(&format!("# {name}\n\n"));
    match head.what {
        What::Conversation => out.push_str(&format!(
            "Everything said in **{}**, conversation {}, as the room wrote it\ndown.",
            head.room, head.conversation
        )),
        What::Recording { secs } => out.push_str(&format!(
            "What was said in **{}** while it was being recorded — {} of it,\nin conversation {}.",
            head.room,
            length(secs),
            head.conversation
        )),
    }
    out.push_str(&format!(
        " Started {date} {time}; {}; times are {}.\n",
        lines(entries.len()),
        zone(head.clock)
    ));
    out.push_str(&format!(
        "Read in {}: a line said in another language is given in {} with the\noriginal under it.\n\n---\n",
        head.language, head.language
    ));
    for e in entries {
        // A line said on the stage is not the room talking, and the
        // conversation already keeps the whole script of a play in one
        // `Kind::Programme` entry at the curtain. Letting both through
        // would print the play twice.
        if e.kind == Kind::Line {
            continue;
        }
        out.push('\n');
        entry(head, e, &mut out);
    }
    out
}

/// What to call the file. The room, the conversation's number and the
/// day: a name that is still obvious in a downloads folder a month
/// later, and that never collides with the next one.
pub fn file_name(head: &Head) -> String {
    let (date, _) = stamp(head.at, head.clock);
    let what = match head.what {
        What::Conversation => "transcript",
        What::Recording { .. } => "recording",
    };
    format!("{}-{}-{date}-{what}.md", slug(head.room), head.conversation)
}

/// One entry, as the paragraph it is.
fn entry(head: &Head, e: &Entry, out: &mut String) {
    let (_, time) = stamp(e.at, head.clock);
    let (text, original) = said(head, e);
    // The one entry nobody said: the room writing down that it stopped
    // being written down. No name on it, because its `who` is the
    // person the switch happened by and not its author.
    if e.kind == Kind::Note {
        out.push_str(&format!("*{time} — {}*\n", one_line(text)));
        if let Some(said) = original {
            out.push_str(&format!("*as the room put it: {}*\n", one_line(said)));
        }
        return;
    }
    let who = format!("**{time} · {}**{}", e.who, badges(e));
    match e.kind {
        // Somebody talking, or an AI answering. Where the AI was
        // talked over the room has already put the dash on the end of
        // the words themselves, so there is nothing to add for it here.
        Kind::Human | Kind::Agent | Kind::Line => match original {
            // Two spaces: a hard break, so the original is a line of
            // its own wherever this is read rather than a clause
            // tacked on to the end of the translation.
            Some(said) => out.push_str(&format!(
                "{who} — {}  \n*as said:* {}\n",
                one_line(text),
                one_line(said)
            )),
            None => out.push_str(&format!("{who} — {}\n", one_line(text))),
        },
        Kind::Game => {
            out.push_str(&format!("{who} saved a game — **{}**\n", one_line(text)));
        }
        Kind::Card | Kind::Programme => {
            let what = if e.kind == Kind::Programme {
                "the play"
            } else {
                "a card"
            };
            out.push_str(&format!(
                "{who} put up {what} — **{}**\n",
                one_line(&e.title)
            ));
            // Quoted rather than let loose: a card is markdown of its
            // own, and a heading or a table in it would otherwise
            // restructure the transcript it landed in.
            quoted(text, out);
        }
        Kind::Picture => {
            out.push_str(&format!(
                "{who} drew picture {}{}  \n",
                one_line(text),
                asked(&e.title)
            ));
            out.push_str("*The picture is not in this file.*\n");
        }
        Kind::Upload => {
            out.push_str(&format!(
                "{who} brought in a picture — {} (picture {})  \n",
                named(&e.title, "a picture"),
                one_line(text)
            ));
            out.push_str("*The picture is not in this file.*\n");
        }
        Kind::Sample => {
            out.push_str(&format!(
                "{who} read a passage aloud — clip {}",
                one_line(text)
            ));
            // What they were given to read, which is the whole of what
            // the clip has in it.
            if e.title.trim().is_empty() {
                out.push_str("  \n");
            } else {
                quoted(&e.title, out);
                out.push('\n');
            }
            out.push_str("*The recording is not in this file.*\n");
        }
        // The room itself, recorded. The clip is a card in the
        // conversation and the transcript of the stretch it covers is
        // this same renderer with bounds, so a file that is already
        // one of those does not carry the other.
        Kind::Recording => {
            out.push_str(&format!("{who} recorded the room — {}  \n", one_line(text)));
            out.push_str("*The recording is not in this file.*\n");
        }
        Kind::Clip => {
            out.push_str(&format!(
                "{who} brought in a recording — {} (clip {})  \n",
                named(&e.title, "a recording"),
                one_line(text)
            ));
            out.push_str("*The recording is not in this file.*\n");
        }
        Kind::Note => unreachable!("a note is answered above"),
    }
}

/// What a line says for this reader, and what it was said in where
/// those are two different things.
///
/// The room only ever keeps a translation into a language a line was
/// *not* said in, so a translation into the reader's own language
/// existing is the whole of the question -- there is nothing to work
/// out about who said it or what they speak.
fn said<'a>(head: &Head, e: &'a Entry) -> (&'a str, Option<&'a str>) {
    match e.translations.get(head.language) {
        Some(t) if !t.trim().is_empty() => (t, Some(&e.text)),
        _ => (&e.text, None),
    }
}

/// What is true about a line besides who said it and when.
fn badges(e: &Entry) -> String {
    let mut out = String::new();
    // Typed, so it is exactly what was meant: no recognition error to
    // second-guess, which is worth knowing a month later.
    if e.typed {
        out.push_str(" (typed)");
    }
    match e.seat {
        // A playhouse divides the room into parts that cannot all hear
        // each other, so where a line was said is part of what it
        // means: the box never heard the audience.
        Some(Seat::Box) => out.push_str(" (in the director's box)"),
        Some(Seat::Stage) => out.push_str(" (on the stage)"),
        Some(Seat::Audience) | None => {}
    }
    out
}

/// A card's or a play's own markdown, held inside a quote so it cannot
/// restructure the document it is quoted in.
fn quoted(text: &str, out: &mut String) {
    out.push('\n');
    for line in text.lines() {
        if line.trim().is_empty() {
            out.push_str(">\n");
        } else {
            out.push_str(&format!("> {line}\n"));
        }
    }
}

fn asked(title: &str) -> String {
    match title.trim() {
        "" => String::new(),
        asked => format!(" — {}", one_line(asked)),
    }
}

fn named(title: &str, fallback: &str) -> String {
    match title.trim() {
        "" => fallback.to_string(),
        name => one_line(name),
    }
}

/// Something said, on the one line a transcript gives it. Markdown
/// would read a line break inside a paragraph as a space anyway; doing
/// it here means the file reads the same unrendered.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn lines(n: usize) -> String {
    match n {
        0 => "nothing was said in it".to_string(),
        1 => "1 line".to_string(),
        n => format!("{n} lines"),
    }
}

/// How long a recording runs, to the second: this is the length of a
/// file sitting beside it, which somebody may well be checking against
/// the player, and not the rounded clause a sentence wants.
fn length(secs: f32) -> String {
    let s = secs.max(0.0).round() as u64;
    match (s / 60, s % 60) {
        (0, s) => format!("{s}s"),
        (m, s) => format!("{m}m {s:02}s"),
    }
}

/// The clock the times are in, as an offset nobody has to guess at.
fn zone(minutes: i32) -> String {
    if minutes == 0 {
        return "UTC".to_string();
    }
    let sign = if minutes < 0 { '-' } else { '+' };
    let (h, m) = (minutes.abs() / 60, minutes.abs() % 60);
    format!("UTC{sign}{h:02}:{m:02}")
}

/// A Unix millisecond as a date and a clock time, `minutes` east of
/// UTC. ISO order and nothing but numbers: the only spelling of a date
/// that is the same in every language the room is read in.
fn stamp(ms: u64, minutes: i32) -> (String, String) {
    let secs = ms as i64 / 1000 + minutes as i64 * 60;
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil(days);
    (
        format!("{y:04}-{m:02}-{d:02}"),
        format!("{:02}:{:02}", rest / 3600, (rest % 3600) / 60),
    )
}

/// The civil date `days` after 1970-01-01. Hinnant's algorithm, which
/// is the standard answer and shorter than any calendar table: the eras
/// are 400 years long, which is where the leap rules come out even.
fn civil(days: i64) -> (i64, u32, u32) {
    // Count from 0000-03-01 instead, so a leap day is the last day of
    // the year rather than one in the middle of it.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// A name that is safe in a filename and still readable: letters,
/// digits and single dashes.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "room".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn head(language: &'static str) -> Head<'static> {
        Head {
            room: "Kitchen table",
            conversation: 7,
            title: "",
            at: 1_793_880_120_000,
            language,
            clock: 0,
            what: What::Conversation,
        }
    }

    fn entry_of(kind: Kind, who: &str, text: &str) -> Entry {
        Entry {
            id: format!("{who}-{text}"),
            who: who.into(),
            kind,
            title: String::new(),
            text: text.into(),
            live: false,
            partial: false,
            cut: false,
            typed: false,
            translations: BTreeMap::new(),
            note: String::new(),
            seat: None,
            at: 1_793_880_180_000,
            sent: true,
        }
    }

    #[test]
    fn a_date_is_spelled_in_numbers_in_the_readers_own_clock() {
        // 2026-11-05T11:22:00Z, which is the afternoon in Berlin and
        // the morning in New York.
        assert_eq!(
            stamp(1_793_877_720_000, 0),
            ("2026-11-05".into(), "11:22".into())
        );
        assert_eq!(
            stamp(1_793_877_720_000, 60),
            ("2026-11-05".into(), "12:22".into())
        );
        assert_eq!(
            stamp(1_793_877_720_000, -300),
            ("2026-11-05".into(), "06:22".into())
        );
        // And over a midnight, in both directions.
        assert_eq!(
            stamp(1_793_877_720_000, -12 * 60),
            ("2026-11-04".into(), "23:22".into())
        );
        assert_eq!(
            stamp(1_793_877_720_000, 13 * 60),
            ("2026-11-06".into(), "00:22".into())
        );
        assert_eq!(zone(0), "UTC");
        assert_eq!(zone(-330), "UTC-05:30");
        assert_eq!(zone(60), "UTC+01:00");
        // The one date every calendar gets wrong if it guesses: 2000
        // was a leap year, 1900 was not.
        assert_eq!(civil(11_016), (2000, 2, 29));
        assert_eq!(civil(0), (1970, 1, 1));
    }

    #[test]
    fn what_was_said_is_in_it_with_who_said_it_and_when() {
        let log = vec![
            entry_of(Kind::Human, "Ryan", "The door is locked."),
            entry_of(Kind::Agent, "Vox", "You try the handle. Nothing gives."),
        ];
        let md = markdown(&head("English"), &log);
        assert!(
            md.contains("**12:03 · Ryan** — The door is locked.\n"),
            "{md}"
        );
        assert!(md.contains("**12:03 · Vox** — You try the handle."), "{md}");
        assert!(md.contains("conversation 7"), "{md}");
        assert!(md.contains("2 lines"), "{md}");
    }

    #[test]
    fn a_line_is_read_in_your_language_with_the_original_under_it() {
        let mut said = entry_of(Kind::Human, "Ryan", "The door is locked.");
        said.translations
            .insert("Russian".into(), "Дверь заперта.".into());
        let md = markdown(&head("Russian"), &[said]);
        assert!(md.contains("— Дверь заперта."), "{md}");
        assert!(md.contains("*as said:* The door is locked."), "{md}");
        // And a reader of the language it was said in gets one copy.
        let mut said = entry_of(Kind::Human, "Ryan", "The door is locked.");
        said.translations
            .insert("Russian".into(), "Дверь заперта.".into());
        let md = markdown(&head("English"), &[said]);
        assert!(md.contains("— The door is locked."), "{md}");
        assert!(!md.contains("as said:"), "{md}");
    }

    #[test]
    fn everything_that_is_not_words_still_says_it_was_there() {
        let mut picture = entry_of(Kind::Picture, "Vox", "3");
        picture.title = "a red door, lit from one side".into();
        let mut clip = entry_of(Kind::Sample, "Ryan", "2");
        clip.title = "The quick brown fox.".into();
        let mut card = entry_of(Kind::Card, "Vox", "# Initiative\n\n1. Ryan");
        card.title = "Initiative order".into();
        let md = markdown(&head("English"), &[picture, clip, card]);
        assert!(
            md.contains("drew picture 3 — a red door, lit from one side"),
            "{md}"
        );
        assert!(md.contains("The picture is not in this file."), "{md}");
        assert!(md.contains("read a passage aloud — clip 2"), "{md}");
        assert!(md.contains("The recording is not in this file."), "{md}");
        // A card's own markdown is quoted, so its heading cannot
        // restructure the transcript it landed in.
        assert!(md.contains("put up a card — **Initiative order**"), "{md}");
        assert!(md.contains("> # Initiative"), "{md}");
    }

    /// The gaps are the one thing a transcript cannot leave out: an
    /// unexplained hour of silence is indistinguishable from an hour in
    /// which nobody spoke, which is what the notes are written for.
    #[test]
    fn the_off_the_record_notes_come_through() {
        let note = entry_of(
            Kind::Note,
            "Ryan",
            "Ryan took the room off the record. Nothing said from here is written down.",
        );
        let md = markdown(&head("English"), &[note]);
        assert!(
            md.contains("*12:03 — Ryan took the room off the record."),
            "{md}"
        );
        assert!(
            !md.contains("· Ryan"),
            "a note is not somebody talking: {md}"
        );
    }

    /// The script of a play is kept once, in the programme the
    /// conversation holds, and not again line by line.
    #[test]
    fn the_play_is_in_it_once() {
        let mut programme = entry_of(Kind::Programme, "Vox", "**Thorne.** Who goes there?");
        programme.title = "The Locked Room".into();
        let said = entry_of(Kind::Line, "Thorne", "Who goes there?");
        let md = markdown(&head("English"), &[programme, said]);
        assert!(md.contains("put up the play — **The Locked Room**"), "{md}");
        assert!(md.contains("> **Thorne.** Who goes there?"), "{md}");
        assert!(!md.contains("· Thorne"), "{md}");
    }

    /// The same document with different bounds: what #18 hands to a
    /// conversation beside the clip, and it must name the clip's own
    /// length so the two files can be checked against each other.
    #[test]
    fn a_recording_is_the_same_transcript_of_a_stretch_of_one() {
        let head = Head {
            what: What::Recording { secs: 92.4 },
            ..head("English")
        };
        let md = markdown(&head, &[entry_of(Kind::Human, "Ryan", "Say that again.")]);
        assert!(md.contains("while it was being recorded — 1m 32s"), "{md}");
        assert!(md.contains("— Say that again."), "{md}");
        assert_eq!(file_name(&head), "kitchen-table-7-2026-11-05-recording.md");
        assert_eq!(length(9.2), "9s");
    }

    #[test]
    fn a_file_name_says_which_room_and_which_conversation() {
        assert_eq!(
            file_name(&head("English")),
            "kitchen-table-7-2026-11-05-transcript.md"
        );
        assert_eq!(slug("Ryan's room!"), "ryan-s-room");
        assert_eq!(slug("…"), "room");
    }
}
