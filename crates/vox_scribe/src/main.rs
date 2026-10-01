//! vox_scribe: live transcription in the terminal, saved as markdown.
//!
//! Uses vox_transcribe's three passes: streaming Zipformer partials
//! (ALL CAPS, instant), SenseVoice re-decodes of each utterance, and
//! boundary re-transcription across neighbouring utterances.

mod clipboard;
mod markdown;
mod models;
#[cfg(unix)]
mod once;
mod tui;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use tracing::info;
use vox_audio::{Capture, OpenOptions};
use vox_transcribe::correct::Corrector;
use vox_transcribe::openai::{OpenAiConfig, OpenAiCorrector};
use vox_transcribe::sherpa::{SenseVoice, SenseVoiceConfig, Zipformer, ZipformerConfig};
use vox_transcribe::{
    Change, Engine, EngineConfig, Event, ParagraphMode, StreamingRecognizer, Transcript,
};

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
    /// Markdown output path. Without it nothing is saved to disk.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Add to the output file if it already exists (normally an error).
    #[arg(long)]
    append: bool,
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
    /// Start new paragraphs only on Enter, not on silence (toggle with `m`).
    #[arg(long)]
    manual: bool,
    /// One-shot: record a single manual paragraph until Enter, then
    /// finish every pass, copy the text to the clipboard and exit.
    /// q / Esc / Ctrl-C cancels without copying. Only one runs at a time:
    /// launching another finishes the running one instead.
    #[arg(long)]
    once: bool,
    /// Enable pass 4: LLM proofreading of each paragraph before it is
    /// written. Configured by VOX_SCRIBE_LLM_URL (default
    /// https://api.openai.com/v1), VOX_SCRIBE_LLM_MODEL (required) and
    /// VOX_SCRIBE_LLM_KEY or OPENAI_API_KEY.
    #[arg(long)]
    llm: bool,
    /// Seconds to wait for each pass-4 reply before keeping pass-3 text.
    #[arg(long, default_value_t = 30)]
    llm_timeout: u64,
    /// File of names and terms (one per line) to help pass 4 spell them.
    #[arg(long)]
    vocab: Option<PathBuf>,
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
    /// Finish the running --once recorder (as if Enter were pressed).
    /// Exits 1 if none is running.
    StopOnce,
}

/// Audio source feeding the engine.
enum Source {
    Live(Capture),
    File(Vec<f32>),
}

/// Everything the UI loops need.
pub struct Session {
    pub engine: Engine,
    /// None until -o or the TUI's `s` names a file.
    pub writer: Mutex<Option<MarkdownWriter>>,
    /// Heading and date line for a file started with `s`.
    pub title: String,
    pub subtitle: String,
    pub device: String,
    pub paused: Arc<AtomicBool>,
    /// Paragraphs break only on Enter.
    pub manual: AtomicBool,
    /// Pass 4 is enabled.
    pub llm: bool,
    /// --once: Enter finishes the session instead of breaking a paragraph.
    pub once: bool,
    /// --once: set by SIGUSR1 (another launch) to finish like Enter.
    pub finish_requested: Arc<AtomicBool>,
    pub stop: Arc<AtomicBool>,
    /// Set when a file input has been fully pushed.
    pub source_done: Arc<AtomicBool>,
    /// The live input; None for a file, or while paused (mic released).
    pub capture: Arc<Mutex<Option<Capture>>>,
    /// Tail of the existing file when appending, shown greyed out.
    pub history: Vec<markdown::HistoryBlock>,
    pump: Option<std::thread::JoinHandle<()>>,
}

impl Session {
    /// Stop the audio, drain every pass, append what's left to the markdown.
    pub fn finish(mut self) -> Result<Transcript> {
        self.stop.store(true, Ordering::SeqCst);
        self.capture.lock().expect("capture lock").take();
        if let Some(p) = self.pump.take() {
            let _ = p.join();
        }
        let t = self.engine.finish();
        if let Some(w) = self.writer.lock().expect("writer lock").as_mut() {
            w.append_remaining(&t)?;
        }
        Ok(t)
    }

    /// Append a hardened paragraph to the markdown file, if there is one.
    pub fn write(&self, p: &vox_transcribe::Paragraph) -> Result<()> {
        match self.writer.lock().expect("writer lock").as_mut() {
            Some(w) => w.append(p),
            None => Ok(()),
        }
    }

    pub fn path(&self) -> Option<PathBuf> {
        self.writer
            .lock()
            .expect("writer lock")
            .as_ref()
            .map(|w| w.path().to_path_buf())
    }

    /// Start saving to a new file at `path`: write `settled` (the
    /// paragraphs no pass will change) now; later ones append as they settle.
    pub fn save_as(&self, path: PathBuf, settled: &[&vox_transcribe::Paragraph]) -> Result<()> {
        let mut w = MarkdownWriter::create(path, &self.title, &self.subtitle, false)?;
        for p in settled {
            w.append(p)?;
        }
        *self.writer.lock().expect("writer lock") = Some(w);
        Ok(())
    }
}

/// How a session ended.
pub struct Outcome {
    pub transcript: Transcript,
    /// The markdown file, if one was being written.
    pub path: Option<PathBuf>,
    /// --once: Enter was pressed (copy the text) rather than cancelled.
    pub confirmed: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let tui_mode = cli.cmd.is_none() && !cli.no_tui;
    let log_file = init_logging(&cli, tui_mode)?;

    match cli.cmd {
        Some(Cmd::Devices) => return list_devices(),
        Some(Cmd::DownloadModels) => {
            let dir = cli
                .models_dir
                .clone()
                .unwrap_or_else(models::user_models_dir);
            return models::download(&dir);
        }
        Some(Cmd::StopOnce) => {
            #[cfg(unix)]
            if once::signal_finish() {
                return Ok(());
            }
            eprintln!("no vox_scribe --once recorder is running");
            std::process::exit(1);
        }
        None => {}
    }

    let finish_requested = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    let _once_guard = if cli.once {
        if once::signal_finish() {
            eprintln!("a --once recorder is already running; told it to finish");
            return Ok(());
        }
        Some(once::claim(finish_requested.clone())?)
    } else {
        None
    };

    let now = chrono::Local::now();
    let output = cli.output.clone();
    if cli.append && output.is_none() {
        anyhow::bail!("--append needs -o FILE");
    }
    // Fail before loading models or opening the mic.
    if let Some(output) = output.as_ref().filter(|o| o.exists() && !cli.append) {
        anyhow::bail!(
            "{} already exists (pass --append to add to it)",
            output.display()
        );
    }
    let corrector = llm_corrector(&cli)?;
    let llm = corrector.is_some();

    let models_dir = models::resolve(cli.models_dir.as_deref());
    // First run: fetch the models rather than failing.
    if !models::has_sense_voice(&models_dir)
        || (!cli.no_streaming && !models::has_zipformer(&models_dir))
    {
        eprintln!("models not found, downloading them once (about 280 MB)…");
        models::download(&models_dir)?;
    }

    // Put the TUI on screen now so the mic and models load behind it
    // rather than behind a blank terminal. sherpa-onnx's C++ code writes to
    // fd 2 directly; send it to the log while the TUI owns the screen.
    #[cfg(unix)]
    let _stderr_restore = match (&log_file, tui_mode) {
        (Some(f), true) => Some(vox_transcribe::stderr::redirect_to(f)?),
        _ => None,
    };
    let screen = tui_mode.then(tui::Screen::splash);

    // Open the mic before loading models so speech during the load is
    // buffered (the capture queue holds ~10 s) rather than lost.
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


    let title = cli.title.clone().unwrap_or_else(|| match &cli.input {
        Some(p) => p
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        None => format!("Transcript {}", now.format("%Y-%m-%d %H:%M")),
    });
    let subtitle = format!("{} · {}", now.format("%Y-%m-%d %H:%M"), device);
    let history = match &output {
        Some(o) if cli.append => markdown::read_tail(o, 200),
        _ => Vec::new(),
    };
    let writer = match output {
        Some(o) => Some(MarkdownWriter::create(
            o, &title, &subtitle, cli.append,
        )?),
        None => None,
    };

    let manual = cli.manual || cli.once;
    let mut engine_cfg = EngineConfig::new(rate);
    if manual {
        engine_cfg.paragraph.mode = ParagraphMode::Manual;
    }
    let engine = Engine::spawn_with(engine_cfg, streaming, Arc::new(offline), corrector);
    let paused = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let source_done = Arc::new(AtomicBool::new(false));

    let (capture, pump) = {
        let pusher = engine.pusher();
        let (paused, stop, done) = (paused.clone(), stop.clone(), source_done.clone());
        match source {
            Source::Live(cap) => {
                let capture = Arc::new(Mutex::new(Some(cap)));
                let live = LivePump {
                    capture: capture.clone(),
                    opts: OpenOptions {
                        device: cli.device.clone(),
                        virtual_sink: cli.virtual_sink,
                    },
                    rate,
                    pusher,
                    paused,
                    stop,
                };
                (capture, std::thread::spawn(move || live.run()))
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
                (Arc::new(Mutex::new(None)), pump)
            }
        }
    };

    let session = Session {
        engine,
        writer: Mutex::new(writer),
        title,
        subtitle,
        device,
        paused,
        manual: AtomicBool::new(manual),
        llm,
        once: cli.once,
        finish_requested,
        stop,
        source_done,
        capture,
        history,
        pump: Some(pump),
    };
    let outcome = match screen {
        Some(screen) => tui::run(screen, session)?,
        None => headless(session)?,
    };
    if cli.once {
        copy_transcript(&outcome);
    }
    if let Some(path) = &outcome.path {
        println!("saved {}", path.display());
    }
    Ok(())
}

/// --once: print the finished text and put it on the clipboard.
fn copy_transcript(outcome: &Outcome) {
    if !outcome.confirmed {
        eprintln!("cancelled, nothing copied");
        return;
    }
    let text = outcome
        .transcript
        .paragraphs
        .iter()
        .map(|p| p.text.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if text.is_empty() {
        eprintln!("nothing transcribed, clipboard unchanged");
        return;
    }
    println!("{text}");
    let words = text.split_whitespace().count();
    match clipboard::copy(&text) {
        Ok(how) => eprintln!("copied {words} words ({how})"),
        Err(e) => eprintln!("copy failed: {e}"),
    }
}

/// Feeds mic audio to the engine. While paused it releases the mic (so
/// the OS stops showing it in use) and pushes silence on wall-clock time,
/// keeping timestamps aligned; on resume it reopens the device.
struct LivePump {
    capture: Arc<Mutex<Option<Capture>>>,
    opts: OpenOptions,
    rate: u32,
    pusher: vox_transcribe::Pusher,
    paused: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl LivePump {
    fn run(self) {
        let mut chunks = self.chunks();
        // Silence pushed since the mic was released: (since, samples).
        let mut silent: Option<(std::time::Instant, u64)> = None;
        while !self.stop.load(Ordering::Relaxed) {
            if self.paused.load(Ordering::Relaxed) {
                // A virtual sink stays up so apps playing into it aren't
                // rerouted; its audio is replaced with silence.
                if chunks.is_some() && !self.opts.virtual_sink {
                    chunks = None;
                    self.capture.lock().expect("capture lock").take();
                    info!("paused: microphone released");
                }
                if let Some(rx) = &chunks {
                    if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(100)) {
                        self.pusher.push(&vec![0.0; chunk.len()]);
                    }
                    continue;
                }
                let (since, pushed) = silent.get_or_insert((std::time::Instant::now(), 0));
                self.catch_up(*since, pushed);
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            if chunks.is_none() {
                match self.reopen() {
                    Ok(rx) => chunks = Some(rx),
                    Err(e) => {
                        tracing::warn!("reopening the microphone failed: {e:#}");
                        self.paused.store(true, Ordering::SeqCst);
                        continue;
                    }
                }
                // Cover the time spent reopening.
                if let Some((since, mut pushed)) = silent.take() {
                    self.catch_up(since, &mut pushed);
                }
            }
            let Some(rx) = &chunks else { continue };
            if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(100)) {
                self.pusher.push(&chunk);
            }
        }
    }

    fn chunks(&self) -> Option<crossbeam_channel::Receiver<Vec<f32>>> {
        let cap = self.capture.lock().expect("capture lock");
        cap.as_ref().map(|c| c.chunks().clone())
    }

    fn reopen(&self) -> Result<crossbeam_channel::Receiver<Vec<f32>>> {
        let cap = vox_audio::default_backend()?.open(&self.opts)?;
        if cap.sample_rate != self.rate {
            anyhow::bail!(
                "device now runs at {} Hz, session started at {} Hz",
                cap.sample_rate,
                self.rate
            );
        }
        let rx = cap.chunks().clone();
        *self.capture.lock().expect("capture lock") = Some(cap);
        info!("resumed: microphone reopened");
        Ok(rx)
    }

    /// Push silence up to the wall-clock time elapsed since `since`.
    fn catch_up(&self, since: std::time::Instant, pushed: &mut u64) {
        let due = (since.elapsed().as_secs_f64() * self.rate as f64) as u64;
        if due > *pushed {
            self.pusher.push(&vec![0.0; (due - *pushed) as usize]);
            *pushed = due;
        }
    }
}

fn headless(session: Session) -> Result<Outcome> {
    let interrupted = Arc::new(AtomicBool::new(false));
    {
        let i = interrupted.clone();
        ctrlc::set_handler(move || i.store(true, Ordering::SeqCst))
            .context("install Ctrl-C handler")?;
    }
    // --once: a line on stdin (Enter) finishes the recording.
    let entered = Arc::new(AtomicBool::new(false));
    if session.once {
        let e = entered.clone();
        eprintln!("recording; press Enter to finish and copy, Ctrl-C to cancel");
        std::thread::spawn(move || {
            let mut line = String::new();
            if std::io::stdin().read_line(&mut line).is_ok_and(|n| n > 0) {
                e.store(true, Ordering::SeqCst);
            }
        });
    }
    let events = session.engine.events().clone();
    let mut printed = HashSet::new();
    let print = |p: &vox_transcribe::Paragraph, printed: &mut HashSet<String>| {
        if !p.text.trim().is_empty() && printed.insert(p.id.clone()) {
            println!("[{}] {}\n", timestamp(p.start_ms), p.text.trim());
        }
    };
    while !interrupted.load(Ordering::SeqCst)
        && !entered.load(Ordering::SeqCst)
        && !session.finish_requested.load(Ordering::SeqCst)
        && !session.source_done.load(Ordering::SeqCst)
    {
        if let Ok(Event::Paragraph {
            paragraph,
            change: Change::Hardened,
        }) = events.recv_timeout(Duration::from_millis(100))
        {
            if !session.once {
                print(&paragraph, &mut printed);
            }
            session.write(&paragraph)?;
        }
    }
    let once = session.once;
    let confirmed = !interrupted.load(Ordering::SeqCst);
    let path = session.path();
    let t = session.finish()?;
    if !once {
        for p in &t.paragraphs {
            print(p, &mut printed);
        }
    }
    Ok(Outcome {
        transcript: t,
        path,
        confirmed,
    })
}

/// Build the pass-4 corrector when `--llm` is given.
fn llm_corrector(cli: &Cli) -> Result<Option<Arc<dyn Corrector>>> {
    if !cli.llm {
        return Ok(None);
    }
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    let url = env("VOX_SCRIBE_LLM_URL").unwrap_or_else(|| "https://api.openai.com/v1".into());
    let model = env("VOX_SCRIBE_LLM_MODEL").context("--llm needs VOX_SCRIBE_LLM_MODEL set")?;
    let api_key = env("VOX_SCRIBE_LLM_KEY").or_else(|| env("OPENAI_API_KEY"));
    let vocabulary = match &cli.vocab {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("read {}", path.display()))?
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(String::from)
            .collect(),
        None => Vec::new(),
    };
    info!(url = %url, model = %model, vocab = vocabulary.len(), "pass 4 enabled");
    let corrector = OpenAiCorrector::new(OpenAiConfig {
        base_url: url.clone(),
        model: model.clone(),
        api_key,
        vocabulary,
        timeout: Duration::from_secs(cli.llm_timeout),
    });
    eprintln!("checking LLM endpoint {url} ({model})…");
    corrector
        .check()
        .with_context(|| format!("pass 4 endpoint check failed ({url}, model {model})"))?;
    Ok(Some(Arc::new(corrector)))
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

/// Returns the log file in TUI mode, for redirecting fd 2 into it.
fn init_logging(cli: &Cli, tui_mode: bool) -> Result<Option<std::fs::File>> {
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
            let for_tui = if tui_mode {
                Some(file.try_clone()?)
            } else {
                None
            };
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(file))
                .init();
            Ok(for_tui)
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
            Ok(None)
        }
    }
}
