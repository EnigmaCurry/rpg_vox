//! Append-only markdown output. The header is written at start and each
//! paragraph is appended once it hardens (no further pass will change
//! it), so `tail -f` and file watchers see ordinary appends.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context as _, Result};
use vox_transcribe::{Paragraph, Transcript};

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
            write!(file, "\n---\n\n*{subtitle}*\n")?;
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

fn header(title: &str, subtitle: &str) -> String {
    format!("# {title}\n\n*{subtitle}*\n")
}

/// A paragraph as written: `**[hh:mm:ss]** text`, or
/// `**[hh:mm:ss] Speaker A:** text` once diarized.
fn block(p: &Paragraph) -> String {
    let ts = timestamp(p.start_ms);
    match &p.speaker {
        Some(s) => format!(
            "\n**[{ts}] {}:** {}\n",
            crate::speakers::name(s),
            p.text.trim()
        ),
        None => format!("\n**[{ts}]** {}\n", p.text.trim()),
    }
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
