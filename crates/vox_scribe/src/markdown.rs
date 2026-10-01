//! Markdown output, rewritten atomically so a crash loses at most the
//! paragraph still being spoken.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use vox_transcribe::Transcript;

pub struct MarkdownWriter {
    path: PathBuf,
    title: String,
    subtitle: String,
}

impl MarkdownWriter {
    pub fn new(path: PathBuf, title: String, subtitle: String) -> Self {
        Self {
            path,
            title,
            subtitle,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write every hardened paragraph (or all of them when `all`).
    pub fn save(&self, t: &Transcript, all: bool) -> Result<()> {
        let mut out = format!("# {}\n\n*{}*\n", self.title, self.subtitle);
        for p in t.paragraphs.iter().filter(|p| all || p.hardened) {
            let text = p.text.trim();
            if text.is_empty() {
                continue;
            }
            out.push_str(&format!("\n**[{}]** {}\n", timestamp(p.start_ms), text));
        }
        let tmp = self.path.with_extension("md.tmp");
        let mut f =
            std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(out.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("write {}", self.path.display()))?;
        Ok(())
    }
}

/// `hh:mm:ss` from milliseconds.
pub fn timestamp(ms: u64) -> String {
    let s = ms / 1000;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}
