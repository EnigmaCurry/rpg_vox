//! The final diarization pass: once a session ends, a full offline
//! diarization of the whole audio replaces the live speaker labels, and
//! the files written during the session are rewritten with the result.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use ratatui::crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::widgets::{Block, Paragraph};
use vox_audio::opus_file::{OpusFile, RATE};
use vox_transcribe::diarize::{relabel, Segment};
use vox_transcribe::sherpa::{Diarizer, SpeakerModels};
use vox_transcribe::Transcript;

use crate::markdown;
use crate::subtitles::{self, Format};
use crate::tui::Screen;

/// Where the session's audio is, for the final pass.
pub enum Audio {
    /// `-i`: already decoded.
    Samples(Vec<f32>, u32),
    /// `--record`: the .opus written during the session.
    Opus(PathBuf),
}

impl Audio {
    fn load(self) -> Result<(Vec<f32>, u32)> {
        match self {
            Audio::Samples(s, rate) => Ok((s, rate)),
            Audio::Opus(path) => {
                let file = OpusFile::open(&path)?;
                let mut cursor = file.cursor(0)?;
                let mut out = Vec::with_capacity(file.len as usize);
                while cursor.read(&mut out)? {}
                Ok((out, RATE))
            }
        }
    }
}

/// The files to rewrite. All were written during the session.
pub struct Files {
    pub md: Option<PathBuf>,
    pub srt: Option<PathBuf>,
    pub ass: Option<PathBuf>,
    pub title: String,
    pub subtitle: String,
}

/// Run the diarization and rewrite `files`. Returns the relabelled
/// transcript, or `None` when the user skipped it (TUI: q / Esc), in
/// which case the files keep their live labels.
pub fn run(
    models: SpeakerModels,
    num_speakers: Option<usize>,
    audio: Audio,
    transcript: &Transcript,
    files: &Files,
    tui: bool,
) -> Result<Option<Transcript>> {
    let started = Instant::now();
    let job = std::thread::spawn(move || -> Result<Vec<Segment>> {
        let (samples, rate) = audio.load()?;
        Diarizer::open(&models, num_speakers)?.process(&samples, rate)
    });
    let segments = if tui {
        let Some(s) = wait_on_screen(job, started) else {
            return Ok(None);
        };
        s
    } else {
        eprintln!("diarizing the recording (this can take a while)…");
        job.join()
            .map_err(|_| anyhow::anyhow!("diarization thread panicked"))?
    }?;
    let t = relabel(transcript, &segments);
    if let Some(p) = &files.md {
        replace(p, &markdown::render(&files.title, &files.subtitle, &t))?;
    }
    if let Some(p) = &files.srt {
        replace(p, &subtitles::render(Format::Srt, &t))?;
    }
    if let Some(p) = &files.ass {
        replace(p, &subtitles::render(Format::Ass, &t))?;
    }
    Ok(Some(t))
}

/// Show elapsed time until `job` finishes. `None` if the user skipped
/// (the job is left to die with the process).
fn wait_on_screen<T>(
    job: std::thread::JoinHandle<Result<T>>,
    started: Instant,
) -> Option<Result<T>> {
    let mut screen = Screen::enter(" diarizing…");
    while !job.is_finished() {
        let secs = started.elapsed().as_secs();
        let msg = format!(
            "Working out who spoke when across the whole recording… {}:{:02}\n\n\
             The files already have the live speaker labels; this replaces them\n\
             with the full-quality ones. q / Esc skips it.",
            secs / 60,
            secs % 60
        );
        let _ = screen.terminal().draw(|f| {
            let p = Paragraph::new(msg).block(Block::bordered().title(" vox_scribe "));
            f.render_widget(p, f.area());
        });
        if event::poll(Duration::from_millis(200)).unwrap_or(false) {
            if let Ok(TermEvent::Key(k)) = event::read() {
                let ctrl_c =
                    k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL);
                if k.kind == KeyEventKind::Press
                    && (ctrl_c || matches!(k.code, KeyCode::Char('q') | KeyCode::Esc))
                {
                    return None;
                }
            }
        }
    }
    screen.restore();
    Some(
        job.join()
            .unwrap_or_else(|_| Err(anyhow::anyhow!("diarization thread panicked"))),
    )
}

/// Write `content` to `path` via a temporary file and a rename, so the
/// old file stays intact until the new one is complete.
fn replace(path: &Path, content: &str) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, content).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))
}
