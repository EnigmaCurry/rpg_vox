//! Append-only markdown output. The header is written at start and each
//! paragraph is appended once it hardens (no further pass will change
//! it), so `tail -f` and file watchers see ordinary appends.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context as _, Result};
use vox_transcribe::{Paragraph, Transcript, Translation};

pub struct MarkdownWriter {
    path: PathBuf,
    file: File,
    written: HashSet<String>,
}

impl MarkdownWriter {
    /// Open `path` for this session. A new file gets the heading. An
    /// existing file is an error unless `append`, in which case the
    /// session starts after a `---` rule with its own date line.
    pub fn create(path: PathBuf, title: &str, subtitle: &str, append: bool) -> Result<Self> {
        let existing = path.metadata().map(|m| m.len() > 0).unwrap_or(false);
        let mut opts = OpenOptions::new();
        if append {
            opts.append(true).create(true);
        } else {
            opts.write(true).create_new(true);
        }
        let mut file = opts.open(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                anyhow!(
                    "{} already exists (pass --append to add to it)",
                    path.display()
                )
            } else {
                anyhow!("create {}: {e}", path.display())
            }
        })?;
        if append && existing {
            write!(file, "\n---\n\n{}\n", wrap(&format!("*{subtitle}*")))?;
        } else {
            file.write_all(header(title, subtitle).as_bytes())?;
        }
        file.sync_data()?;
        Ok(Self {
            path,
            file,
            written: HashSet::new(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append `p` unless it was already written or is empty. Synced to
    /// disk so a crash can't lose a settled paragraph.
    pub fn append(&mut self, p: &Paragraph) -> Result<()> {
        let text = p.text.trim();
        if text.is_empty() || !self.written.insert(p.id.clone()) {
            return Ok(());
        }
        self.file
            .write_all(block(p).as_bytes())
            .and_then(|_| self.file.sync_data())
            .with_context(|| format!("write {}", self.path.display()))
    }

    /// Append every paragraph not yet written (end of session).
    pub fn append_remaining(&mut self, t: &Transcript) -> Result<()> {
        for p in &t.paragraphs {
            self.append(p)?;
        }
        Ok(())
    }
}

pub fn header(title: &str, subtitle: &str) -> String {
    format!("# {title}\n\n{}\n", wrap(&format!("*{subtitle}*")))
}

/// Lines are wrapped to fit within this many columns.
const WIDTH: usize = 79;

/// Word-wrap `text` to [`WIDTH`] columns. A word longer than that gets
/// a line of its own.
pub fn wrap(text: &str) -> String {
    wrap_to(text, WIDTH)
}

/// [`wrap`] to a width of its caller's choosing, for text that is
/// written inside a prefix.
fn wrap_to(text: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    let mut out = String::new();
    let mut col = 0;
    for word in text.split_whitespace() {
        let w = word.width();
        if col > 0 && col + 1 + w > width {
            out.push('\n');
            col = 0;
        } else if col > 0 {
            out.push(' ');
            col += 1;
        }
        out.push_str(word);
        col += w;
    }
    out
}

/// A paragraph as written: `**[hh:mm:ss]** text`, or
/// `**[hh:mm:ss] Speaker A:** text` once diarized, wrapped to [`WIDTH`].
///
/// A paragraph that was translated keeps what was said as the paragraph
/// and gets the reading as a blockquote under it. That way round because
/// what was said is the record: the timestamps and the subtitles are
/// about it, and a reading that looks wrong can only be judged against
/// the line it came from.
fn block(p: &Paragraph) -> String {
    let ts = timestamp(p.start_ms);
    let line = match &p.speaker {
        Some(s) => format!("**[{ts}] {}:** {}", crate::speakers::name(s), p.text.trim()),
        None => format!("**[{ts}]** {}", p.text.trim()),
    };
    let mut out = format!("\n{}\n", wrap(&line));
    if let Some(read) = p.translation.as_ref().and_then(Translation::text) {
        out.push_str(&quote(read));
    }
    out
}

/// `text` as a markdown blockquote, wrapped to [`WIDTH`] with the `> `
/// counted in it.
fn quote(text: &str) -> String {
    let body = wrap_to(text.trim(), WIDTH - 2);
    let mut out = String::from("\n");
    for line in body.lines() {
        out.push_str("> ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// A whole file for `t`, as [`MarkdownWriter`] would have written it.
pub fn render(title: &str, subtitle: &str, t: &Transcript) -> String {
    let mut out = header(title, subtitle);
    for p in t.paragraphs.iter().filter(|p| !p.text.trim().is_empty()) {
        out.push_str(&block(p));
    }
    out
}

/// One block of a previous session's markdown, for showing as context
/// when appending.
pub enum HistoryBlock {
    Paragraph {
        timestamp: String,
        text: String,
    },
    /// A blockquote under a paragraph: its reading in the native
    /// language, with the `> ` markers taken off.
    Reading(String),
    /// A `---` session separator.
    Rule,
    /// An italic date line or other text.
    Note(String),
}

/// The last `max` blocks of an existing transcript (empty if unreadable).
pub fn read_tail(path: &Path, max: usize) -> Vec<HistoryBlock> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let blocks: Vec<HistoryBlock> = content
        .split("\n\n")
        .map(|b| b.trim())
        .filter(|b| !b.is_empty() && !b.starts_with("# "))
        .map(|b| {
            if b == "---" {
                return HistoryBlock::Rule;
            }
            if b.starts_with("> ") {
                let text = b
                    .lines()
                    .map(|l| l.trim_start_matches("> ").trim())
                    .collect::<Vec<_>>()
                    .join(" ");
                return HistoryBlock::Reading(text);
            }
            if let Some((head, text)) = b.strip_prefix("**[").and_then(|r| r.split_once("** ")) {
                // `ts]` or `ts] Speaker A:`
                let (ts, who) = head.split_once(']').unwrap_or((head, ""));
                let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                return HistoryBlock::Paragraph {
                    timestamp: ts.to_string(),
                    text: match who.trim() {
                        "" => text,
                        who => format!("{who} {text}"),
                    },
                };
            }
            HistoryBlock::Note(b.trim_matches('*').to_string())
        })
        .collect();
    let skip = blocks.len().saturating_sub(max);
    blocks.into_iter().skip(skip).collect()
}

/// `hh:mm:ss` from milliseconds.
pub fn timestamp(ms: u64) -> String {
    let s = ms / 1000;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_before_80_columns() {
        let text = "word ".repeat(40);
        let out = wrap(&format!("**[00:00:01] Speaker A:** {text}"));
        assert!(out.lines().count() > 1);
        assert!(out.lines().all(|l| l.chars().count() <= WIDTH));
        assert_eq!(
            out.split_whitespace().collect::<Vec<_>>(),
            format!("**[00:00:01] Speaker A:** {text}")
                .split_whitespace()
                .collect::<Vec<_>>()
        );
        assert_eq!(wrap("short line"), "short line");
        let long = "x".repeat(100);
        assert_eq!(wrap(&format!("a {long} b")), format!("a\n{long}\nb"));
    }

    #[test]
    fn a_reading_is_written_under_the_line_it_was_made_from() {
        use vox_transcribe::Translation;
        let mut p = Paragraph {
            id: "p1".into(),
            start_ms: 2_000,
            end_ms: 4_000,
            text: "Да, конечно.".into(),
            clips: Vec::new(),
            words: Vec::new(),
            speaker: None,
            closed: true,
            hardened: true,
            pass3_inflight: false,
            pass4: None,
            translation: None,
        };
        // Nothing to read yet, and nothing extra written for it.
        assert_eq!(block(&p), "\n**[00:00:02]** Да, конечно.\n");
        p.translation = Some(Translation::Native { of: p.text.clone() });
        assert_eq!(block(&p), "\n**[00:00:02]** Да, конечно.\n");
        p.translation = Some(Translation::Done {
            of: p.text.clone(),
            language: "Russian".into(),
            text: "Yes, of course.".into(),
            lines: Vec::new(),
        });
        assert_eq!(
            block(&p),
            "\n**[00:00:02]** Да, конечно.\n\n> Yes, of course.\n"
        );
        // The quote wraps with its own markers counted, so no line of
        // the file runs past the width.
        p.translation = Some(Translation::Done {
            of: p.text.clone(),
            language: "Russian".into(),
            text: "word ".repeat(40),
            lines: Vec::new(),
        });
        let out = block(&p);
        assert!(out.lines().all(|l| l.chars().count() <= WIDTH), "{out}");
        assert!(
            out.lines().filter(|l| l.starts_with("> ")).count() > 1,
            "{out}"
        );
    }
}
