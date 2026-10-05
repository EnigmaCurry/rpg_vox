//! The final diarization pass: once a session ends, a full offline
//! diarization of the whole audio replaces the live speaker labels, and
//! the files written during the session are rewritten with the result.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use vox_audio::opus_file::{OpusFile, RATE};
use vox_transcribe::diarize::{relabel_refined, RefineConfig};
use vox_transcribe::sherpa::{Diarizer, DiarizerTuning, SpeakerEmbedder, SpeakerModels};
use vox_transcribe::Transcript;

use crate::markdown;
use crate::progress::Shared;
use crate::subtitles::{self, Format};

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

/// Run the diarization and rewrite `files`, with a progress bar on the
/// console (after a TUI session too: its screen has closed by now).
/// Returns the relabelled transcript. Ctrl-C skips it by quitting, and
/// the files keep their live labels.
pub fn run(
    models: SpeakerModels,
    num_speakers: Option<usize>,
    audio: Audio,
    transcript: &Transcript,
    files: &Files,
    tui: bool,
) -> Result<Transcript> {
    let progress = Arc::new(Shared::default());
    let job = {
        let progress = progress.clone();
        let transcript = transcript.clone();
        std::thread::spawn(move || -> Result<Transcript> {
            let (samples, rate) = audio.load()?;
            // The count is applied afterwards (see relabel_refined): fixed
            // in the clustering, it merges two real voices and keeps a
            // stray as the other "speaker". The 10 s windows step by 5 s,
            // not sherpa's 1 s: embedding them is nearly all the time, and
            // the sentence pass below redoes the fine detail anyway.
            let tuning = DiarizerTuning {
                window_shift_ratio: 0.5,
                ..Default::default()
            };
            let segments = Diarizer::open_tuned(&models, &tuning)?.process_with_progress(
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
                &RefineConfig {
                    max_speakers: num_speakers,
                    ..Default::default()
                },
            ))
        })
    };
    if tui {
        // The TUI has closed; the terminal is back to normal, so Ctrl-C
        // is a plain signal now and nothing has caught it yet.
        let _ = ctrlc::set_handler(|| {
            if crate::CTRL_C_QUITS.load(std::sync::atomic::Ordering::SeqCst) {
                crate::progress::say("diarization skipped; files keep the live speaker labels");
                std::process::exit(130);
            }
        });
    }
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
    let t = t?;
    rewrite(files, &t)?;
    Ok(t)
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

/// Write `content` to `path` via a temporary file and a rename, so the
/// old file stays intact until the new one is complete.
fn replace(path: &Path, content: &str) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, content).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))
}
