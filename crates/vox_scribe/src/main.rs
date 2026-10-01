//! vox_scribe: live transcription in the terminal, saved as markdown.
//!
//! Uses vox_transcribe's three passes: streaming Zipformer partials
//! (ALL CAPS, instant), SenseVoice re-decodes of each utterance, and
//! boundary re-transcription across neighbouring utterances.

mod markdown;
mod models;
mod tui;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use tracing::info;
use vox_audio::{Capture, OpenOptions};
use vox_transcribe::sherpa::{SenseVoice, SenseVoiceConfig, Zipformer, ZipformerConfig};
use vox_transcribe::{Change, Engine, EngineConfig, Event, StreamingRecognizer, Transcript};

use crate::markdown::{timestamp, MarkdownWriter};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
    /// Input device id or name substring (see `devices`). Default: system input.
    #[arg(short, long)]
    device: Option<String>,
    /// PipeWire only: register a virtual sink named `vox_scribe` and
    /// transcribe whatever is played into it.
    #[arg(long)]
    virtual_sink: bool,
    /// Transcribe an audio file (wav, flac, mp3, ogg) instead of a device.
    #[arg(short, long)]
    input: Option<PathBuf>,
    /// Markdown output path. Default: ./transcript-<date>-<time>.md
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Heading for the markdown file.
    #[arg(long)]
    title: Option<String>,
    /// Print finished paragraphs to stdout instead of the full-screen UI.
    #[arg(long)]
    no_tui: bool,
    /// Directory holding sense-voice/ and streaming-zipformer/.
    #[arg(long, env = "VOX_SCRIBE_MODELS")]
    models_dir: Option<PathBuf>,
    /// SenseVoice language: auto, en, zh, ja, ko, yue.
    #[arg(long, default_value = "auto")]
    language: String,
    /// Threads per recognizer.
    #[arg(long, default_value_t = 2)]
    threads: i32,
    /// Skip pass 1 (no live partials; text appears per utterance).
    #[arg(long)]
    no_streaming: bool,
    /// Log file. Default: <tmp>/vox_scribe.log in TUI mode, stderr otherwise.
    #[arg(long)]
    log: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Cmd {
    /// List input devices for the native audio backend.
    Devices,
    /// Download the SenseVoice and Zipformer models.
    DownloadModels,
}

/// Audio source feeding the engine.
enum Source {
    Live(Capture),
    File(Vec<f32>),
}

/// Everything the UI loops need.
pub struct Session {
    pub engine: Engine,
    pub writer: MarkdownWriter,
    pub device: String,
    pub paused: Arc<AtomicBool>,
    pub stop: Arc<AtomicBool>,
    /// Set when a file input has been fully pushed.
    pub source_done: Arc<AtomicBool>,
    pub capture: Option<Capture>,
    pump: Option<std::thread::JoinHandle<()>>,
}

impl Session {
    /// Stop the audio, drain every pass, write the final markdown.
    pub fn finish(mut self) -> Result<Transcript> {
        self.stop.store(true, Ordering::SeqCst);
        self.capture.take();
        if let Some(p) = self.pump.take() {
            let _ = p.join();
        }
        let t = self.engine.finish();
        self.writer.save(&t, true)?;
        Ok(t)
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let tui_mode = cli.cmd.is_none() && !cli.no_tui;
    init_logging(&cli, tui_mode)?;

    match cli.cmd {
        Some(Cmd::Devices) => return list_devices(),
        Some(Cmd::DownloadModels) => {
            let dir = cli
                .models_dir
                .clone()
                .unwrap_or_else(models::user_models_dir);
            return models::download(&dir);
        }
        None => {}
    }

    let models_dir = models::resolve(cli.models_dir.as_deref());
    let mut sv = SenseVoiceConfig::from_dir(&models_dir.join(models::SENSE_VOICE));
    sv.language = cli.language.clone();
    sv.num_threads = cli.threads;
    let offline = SenseVoice::open(&sv).with_context(|| {
        format!(
            "loading SenseVoice from {} (run `vox_scribe download-models`)",
            models_dir.display()
        )
    })?;
    let streaming: Option<Box<dyn StreamingRecognizer>> =
        if cli.no_streaming || !models::has_zipformer(&models_dir) {
            None
        } else {
            let mut zc = ZipformerConfig::from_dir(&models_dir.join(models::ZIPFORMER));
            zc.num_threads = cli.threads;
            Some(Box::new(Zipformer::open(&zc)?.session()))
        };

    let (source, rate, device) = match &cli.input {
        Some(path) => {
            let d = vox_audio::file::decode(path)?;
            (
                Source::File(d.samples),
                d.sample_rate,
                path.display().to_string(),
            )
        }
        None => {
            let backend = vox_audio::default_backend()?;
            let cap = backend.open(&OpenOptions {
                device: cli.device.clone(),
                virtual_sink: cli.virtual_sink,
            })?;
            let (rate, name) = (cap.sample_rate, cap.device.clone());
            (Source::Live(cap), rate, name)
        }
    };
    info!(rate, device = %device, "audio source ready");

    let now = chrono::Local::now();
    let output = cli
        .output
        .clone()
        .unwrap_or_else(|| PathBuf::from(now.format("transcript-%Y%m%d-%H%M%S.md").to_string()));
    let title = cli.title.clone().unwrap_or_else(|| match &cli.input {
        Some(p) => p
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        None => format!("Transcript {}", now.format("%Y-%m-%d %H:%M")),
    });
    let subtitle = format!("{} · {}", now.format("%Y-%m-%d %H:%M"), device);
    let writer = MarkdownWriter::new(output, title, subtitle);

    let engine = Engine::spawn(EngineConfig::new(rate), streaming, Arc::new(offline));
    let paused = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let source_done = Arc::new(AtomicBool::new(false));

    let (capture, pump) = {
        let pusher = engine.pusher();
        let (paused, stop, done) = (paused.clone(), stop.clone(), source_done.clone());
        match source {
            Source::Live(cap) => {
                let chunks = cap.chunks().clone();
                let pump = std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let Ok(chunk) = chunks.recv_timeout(Duration::from_millis(100)) else {
                            continue;
                        };
                        // While paused, keep the audio clock running with
                        // silence so timestamps stay aligned to wall time.
                        if paused.load(Ordering::Relaxed) {
                            pusher.push(&vec![0.0; chunk.len()]);
                        } else {
                            pusher.push(&chunk);
                        }
                    }
                });
                (Some(cap), pump)
            }
            Source::File(samples) => {
                let pump = std::thread::spawn(move || {
                    for chunk in samples.chunks((rate as usize / 50).max(1)) {
                        while paused.load(Ordering::Relaxed) && !stop.load(Ordering::Relaxed) {
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        pusher.push(chunk);
                    }
                    done.store(true, Ordering::SeqCst);
                });
                (None, pump)
            }
        }
    };

    let session = Session {
        engine,
        writer,
        device,
        paused,
        stop,
        source_done,
        capture,
        pump: Some(pump),
    };
    let path = session.writer.path().to_path_buf();
    if tui_mode {
        tui::run(session)?;
    } else {
        headless(session)?;
    }
    println!("saved {}", path.display());
    Ok(())
}

fn headless(session: Session) -> Result<()> {
    let interrupted = Arc::new(AtomicBool::new(false));
    {
        let i = interrupted.clone();
        ctrlc::set_handler(move || i.store(true, Ordering::SeqCst))
            .context("install Ctrl-C handler")?;
    }
    let events = session.engine.events().clone();
    let mut printed = HashSet::new();
    let mut mirror = Transcript::default();
    let print = |p: &vox_transcribe::Paragraph, printed: &mut HashSet<String>| {
        if !p.text.trim().is_empty() && printed.insert(p.id.clone()) {
            println!("[{}] {}\n", timestamp(p.start_ms), p.text.trim());
        }
    };
    while !interrupted.load(Ordering::SeqCst) && !session.source_done.load(Ordering::SeqCst) {
        match events.recv_timeout(Duration::from_millis(100)) {
            Ok(Event::Paragraph { paragraph, change }) => {
                mirror.upsert(&paragraph);
                if change == Change::Hardened {
                    print(&paragraph, &mut printed);
                    session.writer.save(&mirror, false)?;
                }
            }
            Ok(Event::ParagraphRemoved { id }) => mirror.remove(&id),
            Ok(Event::Level { .. }) | Err(_) => {}
        }
    }
    let t = session.finish()?;
    for p in &t.paragraphs {
        print(p, &mut printed);
    }
    Ok(())
}

fn list_devices() -> Result<()> {
    let backend = vox_audio::default_backend()?;
    println!("{} input devices:", backend.name());
    for d in backend.list_devices()? {
        let mark = if d.is_default { "*" } else { " " };
        println!("{mark} {}\n      id: {}", d.name, d.id);
    }
    Ok(())
}

fn init_logging(cli: &Cli, tui_mode: bool) -> Result<()> {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("VOX_SCRIBE_LOG").unwrap_or_else(|_| {
        EnvFilter::new("warn,vox_scribe=info,vox_transcribe=info,vox_audio=info")
    });
    let log_path = cli
        .log
        .clone()
        .or_else(|| tui_mode.then(|| std::env::temp_dir().join("vox_scribe.log")));
    match log_path {
        Some(path) => {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("open log {}", path.display()))?;
            // sherpa-onnx's C++ code writes to fd 2 directly; send it to
            // the log too so it can't draw over the TUI.
            #[cfg(unix)]
            if tui_mode {
                vox_transcribe::stderr::redirect_to(&file)?;
            }
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(file))
                .init();
        }
        None => {
            #[cfg(unix)]
            let ansi = vox_transcribe::stderr::install();
            #[cfg(not(unix))]
            let ansi = true;
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(ansi)
                .with_writer(std::io::stderr)
                .init();
        }
    }
    Ok(())
}
