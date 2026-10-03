//! The final diarization pass: once a session ends, a full offline
//! diarization of the whole audio replaces the live speaker labels, and
//! the files written during the session are rewritten with the result.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use ratatui::crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Gauge, Paragraph};
use vox_audio::opus_file::{OpusFile, RATE};
use vox_transcribe::diarize::{relabel_refined, RefineConfig};
use vox_transcribe::sherpa::{Diarizer, SpeakerEmbedder, SpeakerModels};
use vox_transcribe::Transcript;

use crate::markdown;
use crate::progress::{clock, eta_ms, Shared};
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
    let progress = Arc::new(Shared::default());
    let job = {
        let progress = progress.clone();
        let transcript = transcript.clone();
        std::thread::spawn(move || -> Result<Transcript> {
            let (samples, rate) = audio.load()?;
            let segments = Diarizer::open(&models, num_speakers)?.process_with_progress(
                &samples,
                rate,
                &mut |done, total| progress.set(done, total),
            )?;
            for s in &segments {
                tracing::debug!(
                    start_ms = s.start_ms,
                    end_ms = s.end_ms,
                    speaker = s.speaker,
                    "speaker turn"
                );
            }
            // Tighten the turn boundaries sentence by sentence.
            let embedder = SpeakerEmbedder::open(&models)?;
            let at = |ms: u64| ((ms * rate as u64 / 1000) as usize).min(samples.len());
            let mut embed =
                |start: u64, end: u64| embedder.embed(&samples[at(start)..at(end)], rate);
            Ok(relabel_refined(
                &transcript,
                &segments,
                &mut embed,
                &RefineConfig::default(),
            ))
        })
    };
    let t = if tui {
        let Some(s) = wait_on_screen(job, &progress, started) else {
            return Ok(None);
        };
        s
    } else {
        crate::CTRL_C_QUITS.store(true, std::sync::atomic::Ordering::SeqCst);
        let t = crate::progress::bar_while(
            "finding speaker turns (Ctrl-C skips diarization)",
            "diarizing",
            &progress,
            || job.join(),
        )
        .map_err(|_| anyhow::anyhow!("diarization thread panicked"))?;
        // Don't quit halfway through rewriting the files.
        crate::CTRL_C_QUITS.store(false, std::sync::atomic::Ordering::SeqCst);
        t
    }?;
    rewrite(files, &t)?;
    Ok(Some(t))
}

/// Replace `files` with renderings of `t` (with the current speaker names).
pub fn rewrite(files: &Files, t: &Transcript) -> Result<()> {
    if let Some(p) = &files.md {
        replace(p, &markdown::render(&files.title, &files.subtitle, t))?;
    }
    if let Some(p) = &files.srt {
        replace(p, &subtitles::render(Format::Srt, t))?;
    }
    if let Some(p) = &files.ass {
        replace(p, &subtitles::render(Format::Ass, t))?;
    }
    Ok(())
}

/// Show elapsed time until `job` finishes. `None` if the user skipped
/// (the job is left to die with the process).
fn wait_on_screen<T>(
    job: std::thread::JoinHandle<Result<T>>,
    progress: &Shared,
    started: Instant,
) -> Option<Result<T>> {
    let mut screen = Screen::enter(" diarizing…");
    let mut measuring: Option<Instant> = None;
    while !job.is_finished() {
        let elapsed = clock(started.elapsed().as_millis() as u64);
        let msg = format!(
            "Working out who spoke when across the whole recording… {elapsed}\n\n\
             The files already have the live speaker labels; this replaces them\n\
             with the full-quality ones. q / Esc skips it."
        );
        let gauge = progress.get().map(|(done, total)| {
            let since = *measuring.get_or_insert_with(Instant::now);
            let frac = done as f64 / total as f64;
            let left = eta_ms(frac, since)
                .map(|e| format!("  ~{} left", clock(e)))
                .unwrap_or_default();
            Gauge::default()
                .gauge_style(Style::new().fg(Color::Cyan))
                .ratio(frac.clamp(0.0, 1.0))
                .label(format!("{:.0}%  {done}/{total}{left}", frac * 100.0))
        });
        let _ = screen.terminal().draw(|f| {
            let block = Block::bordered().title(" vox_scribe ");
            let inner = block.inner(f.area());
            f.render_widget(block, f.area());
            let [text, _, bar] = Layout::vertical([
                Constraint::Length(4),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .areas(inner);
            f.render_widget(Paragraph::new(msg), text);
            match gauge {
                Some(g) => f.render_widget(g, bar),
                None => f.render_widget(
                    Paragraph::new("finding speaker turns…")
                        .style(Style::new().fg(Color::DarkGray)),
                    bar,
                ),
            }
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
