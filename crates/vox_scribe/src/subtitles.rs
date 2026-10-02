//! Append-only SRT output, written beside the markdown. Like the
//! markdown, each paragraph's cues are written once it hardens.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;

use anyhow::{anyhow, Context as _, Result};
use vox_transcribe::subtitle::{self, CueConfig};
use vox_transcribe::{Paragraph, Transcript};

/// Silence left between an appended session and the one before it.
const SESSION_GAP_MS: u64 = 2000;

pub struct SubtitleWriter {
    path: PathBuf,
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
    pub fn create(path: PathBuf, append: bool) -> Result<Self> {
        let (cues, offset_ms) = if append {
            match std::fs::read_to_string(&path) {
                Ok(s) => resume_point(&s),
                Err(_) => (0, 0),
            }
        } else {
            (0, 0)
        };
        let mut opts = OpenOptions::new();
        if append {
            opts.append(true).create(true);
        } else {
            opts.write(true).create_new(true);
        }
        let file = opts.open(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                anyhow!(
                    "{} already exists (pass --append to add to it)",
                    path.display()
                )
            } else {
                anyhow!("create {}: {e}", path.display())
            }
        })?;
        Ok(Self {
            path,
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
            out.push_str(&subtitle::srt(self.cues, &cue));
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

/// Cue count and start offset for a session appended to `srt`.
fn resume_point(srt: &str) -> (usize, u64) {
    let mut cues = 0;
    let mut last_end = None;
    for line in srt.lines() {
        if let Some((_, end)) = line.split_once(" --> ") {
            cues += 1;
            last_end = parse_clock(end.trim()).or(last_end);
        }
    }
    (cues, last_end.map(|e| e + SESSION_GAP_MS).unwrap_or(0))
}

/// `hh:mm:ss,mmm` to ms.
fn parse_clock(s: &str) -> Option<u64> {
    let (hms, ms) = s.split_once(',')?;
    let mut parts = hms.split(':').map(|p| p.parse::<u64>().ok());
    let (h, m, sec) = (parts.next()??, parts.next()??, parts.next()??);
    Some(((h * 60 + m) * 60 + sec) * 1000 + ms.parse::<u64>().ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resumes_after_last_cue() {
        let srt = "1\n00:00:00,000 --> 00:00:02,480\nHi.\n\n2\n01:00:03,020 --> 01:00:07,340\nThere.\n\n";
        assert_eq!(resume_point(srt), (2, 3_607_340 + SESSION_GAP_MS));
        assert_eq!(resume_point(""), (0, 0));
    }
}
