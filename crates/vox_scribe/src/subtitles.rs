//! Append-only subtitle output, written beside the markdown: SRT for
//! ordinary players and ASS karaoke for word-by-word highlighting. Like
//! the markdown, each paragraph's cues are written once it hardens.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;

use anyhow::{anyhow, Context as _, Result};
use vox_transcribe::subtitle::{self, CueConfig};
use vox_transcribe::{Paragraph, Transcript};

/// Silence left between an appended session and the one before it.
const SESSION_GAP_MS: u64 = 2000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Srt,
    /// ASS with a `\k` karaoke tag per word.
    Ass,
}

pub struct SubtitleWriter {
    path: PathBuf,
    format: Format,
    file: File,
    /// Cues written so far, including a previous session's when appending.
    cues: usize,
    /// Added to every time: where this session starts in an appended file.
    offset_ms: u64,
    written: HashSet<String>,
    cfg: CueConfig,
}

impl SubtitleWriter {
    /// Open `path` for this session. An existing file is an error unless
    /// `append`, in which case numbering continues and this session's
    /// cues start just after the previous session's last one.
    pub fn create(path: PathBuf, format: Format, append: bool) -> Result<Self> {
        let existing = if append {
            std::fs::read_to_string(&path).unwrap_or_default()
        } else {
            String::new()
        };
        let (cues, offset_ms) = resume_point(&existing, format);
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
        if format == Format::Ass && existing.trim().is_empty() {
            file.write_all(subtitle::ASS_HEADER.as_bytes())?;
            file.sync_data()?;
        }
        Ok(Self {
            path,
            format,
            file,
            cues,
            offset_ms,
            written: HashSet::new(),
            cfg: CueConfig::default(),
        })
    }

    /// Append `p`'s cues unless it was already written or is empty.
    pub fn append(&mut self, p: &Paragraph) -> Result<()> {
        if p.words.is_empty() || !self.written.insert(p.id.clone()) {
            return Ok(());
        }
        let mut out = String::new();
        for mut cue in subtitle::cues(&p.words, p.end_ms, &self.cfg) {
            cue.start_ms += self.offset_ms;
            cue.end_ms += self.offset_ms;
            self.cues += 1;
            out.push_str(&match self.format {
                Format::Srt => subtitle::srt(self.cues, &cue),
                Format::Ass => subtitle::ass(&cue),
            });
        }
        self.file
            .write_all(out.as_bytes())
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

/// Cue count and start offset for a session appended to `existing`.
fn resume_point(existing: &str, format: Format) -> (usize, u64) {
    let mut cues = 0;
    let mut last_end = None;
    for line in existing.lines() {
        let end = match format {
            Format::Srt => line.split_once(" --> ").map(|(_, e)| e),
            Format::Ass => line
                .strip_prefix("Dialogue:")
                .and_then(|l| l.split(',').nth(2)),
        };
        if let Some(end) = end {
            cues += 1;
            last_end = parse_clock(end.trim()).or(last_end);
        }
    }
    (cues, last_end.map(|e| e + SESSION_GAP_MS).unwrap_or(0))
}

/// `hh:mm:ss,mmm` (SRT) or `h:mm:ss.cc` (ASS) to ms.
fn parse_clock(s: &str) -> Option<u64> {
    let (hms, frac) = s.split_once([',', '.'])?;
    let mut parts = hms.split(':').map(|p| p.parse::<u64>().ok());
    let (h, m, sec) = (parts.next()??, parts.next()??, parts.next()??);
    let frac_ms = frac.parse::<u64>().ok()? * 10u64.pow(3u32.saturating_sub(frac.len() as u32));
    Some(((h * 60 + m) * 60 + sec) * 1000 + frac_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resumes_after_last_cue() {
        let srt =
            "1\n00:00:00,000 --> 00:00:02,480\nHi.\n\n2\n01:00:03,020 --> 01:00:07,340\nThere.\n\n";
        assert_eq!(
            resume_point(srt, Format::Srt),
            (2, 3_607_340 + SESSION_GAP_MS)
        );
        assert_eq!(resume_point("", Format::Srt), (0, 0));
        let ass = "[Events]\nDialogue: 0,0:00:00.28,0:00:01.50,Default,,0,0,0,,{\\k22}Hi\n";
        assert_eq!(resume_point(ass, Format::Ass), (1, 1500 + SESSION_GAP_MS));
    }
}
