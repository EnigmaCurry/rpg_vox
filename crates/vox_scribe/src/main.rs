//! vox_scribe: live transcription in the terminal, saved as markdown.
//!
//! Uses vox_transcribe's three passes: streaming Zipformer partials
//! (ALL CAPS, instant), Parakeet (or SenseVoice) re-decodes of each
//! utterance, and boundary re-transcription across neighbouring
//! utterances. With --diarize each utterance is also labelled with its
//! speaker, and a full diarization relabels the saved files at the end.

mod clipboard;
mod diarize;
mod markdown;
mod models;
#[cfg(unix)]
mod once;
mod play;
mod progress;
mod speakers;
mod subtitles;
mod tui;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use tracing::info;
use vox_audio::{Capture, OpenOptions};
use vox_transcribe::correct::Corrector;
use vox_transcribe::openai::{OpenAiConfig, OpenAiCorrector};
use vox_transcribe::sherpa::{
    EmbeddingTagger, Parakeet, ParakeetConfig, SenseVoice, SenseVoiceConfig, SpeakerModels,
    Zipformer, ZipformerConfig,
};
use vox_transcribe::speaker::ClusterConfig;
use vox_transcribe::{
    Change, Engine, EngineConfig, Event, ParagraphMode, SpeakerTagger, StreamingRecognizer,
    Transcript,
};

use crate::markdown::{timestamp, MarkdownWriter};
use crate::subtitles::{Format, SubtitleWriter};
use vox_audio::opus_file::OpusWriter;

#[derive(Parser)]
#[command(name = "scribe", version, about)]
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
    /// Transcribe what one app is playing instead of an input: a PID or a
    /// name / bundle id substring (see `apps`). The app still plays to
    /// your speakers. macOS 14.4+ (process tap) or PipeWire.
    #[arg(short, long, conflicts_with_all = ["device", "virtual_sink", "input"])]
    app: Option<String>,
    /// Transcribe an audio file (wav, flac, mp3, ogg) instead of a device.
    #[arg(short, long)]
    input: Option<PathBuf>,
    /// With --input: open the TUI and play the file to the speakers,
    /// transcribing it in real time as if it were coming from the mic.
    /// `space` pauses both.
    #[arg(long, requires = "input", conflicts_with = "no_tui")]
    live: bool,
    /// Markdown output: writes NAME.md (`.md` is added if missing).
    /// Without it (or --record) nothing is saved to disk.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Record a session: NAME.md (the transcript), NAME.srt (subtitles),
    /// NAME.ass (karaoke subtitles that highlight each word as it is
    /// spoken) and NAME.opus (the audio). Play it back with --play.
    #[arg(short, long, conflicts_with_all = ["output", "append"])]
    record: Option<PathBuf>,
    /// Play a recording made with --record: NAME.opus with NAME.ass
    /// shown word by word in time with the audio.
    #[arg(short, long, conflicts_with_all = ["output", "record", "input", "app", "device", "virtual_sink", "once", "no_tui"])]
    play: Option<PathBuf>,
    /// Add to the -o file if it already exists (normally an error).
    #[arg(long)]
    append: bool,
    /// Heading for the markdown file.
    #[arg(long)]
    title: Option<String>,
    /// Print finished paragraphs to stdout instead of the full-screen UI.
    /// Always the case with --input (unless --live), which shows brief
    /// progress on stderr.
    #[arg(long)]
    no_tui: bool,
    /// Directory holding the model bundles (sense-voice/, parakeet-tdt-0.6b-v2/, streaming-zipformer/).
    #[arg(long, env = "VOX_SCRIBE_MODELS")]
    models_dir: Option<PathBuf>,
    /// Recognizer for passes 2 and 3 (re-decode and boundary fix).
    #[arg(long, value_enum, default_value = "parakeet", env = "VOX_SCRIBE_MODEL")]
    model: models::Offline,
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
    /// launching another finishes the running one instead. Nothing is
    /// saved to disk.
    #[arg(long, conflicts_with_all = ["output", "append", "record"])]
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
    /// Label who is speaking (Speaker A, B, …). Each utterance is labelled
    /// live as it is transcribed; with --record, or -i with -o, a full
    /// diarization of the whole audio then runs after the session and
    /// rewrites the files with the final labels. Fetches ~77 MB of
    /// speaker models on first use.
    #[arg(long)]
    diarize: bool,
    /// With --diarize: who is speaking, when known. Either how many people
    /// ("3") or their names in the order they are first heard
    /// ("Alice,Bob,Carol", which also means 3). The count is exact: every
    /// voice goes to one of them. Rename speakers any time with `n` in
    /// the TUI.
    #[arg(long, requires = "diarize", value_parser = parse_speakers)]
    speakers: Option<Speakers>,
    /// With --diarize: how alike (cosine similarity, 0–1) an utterance's
    /// voice must be to a known speaker to get their live label; higher
    /// tells similar voices apart but may split one person in two. Only
    /// the live labels use it.
    #[arg(long, requires = "diarize", default_value_t = ClusterConfig::default().threshold)]
    speaker_threshold: f32,
    /// File of names and terms the recognizer can't know (one per line,
    /// `#` comments). With Parakeet, decoding is biased toward them, and
    /// a term is only kept where an unbiased decode heard something that
    /// sounds like it; with --llm, pass 4 also gets the list. Cost: the
    /// re-decode passes run about 20% slower, up to ~1.8x where the names
    /// come up often (live text and diarization are unaffected).
    #[arg(long)]
    vocab: Option<PathBuf>,
    /// Log file. Default: <tmp>/vox_scribe.log in TUI mode, stderr otherwise.
    #[arg(long)]
    log: Option<PathBuf>,
}

/// `--speakers`: a count, or names (whose number is the count).
#[derive(Clone, Debug, PartialEq)]
struct Speakers {
    count: usize,
    names: Vec<String>,
}

fn parse_speakers(arg: &str) -> Result<Speakers, String> {
    if let Ok(n) = arg.trim().parse::<usize>() {
        if n == 0 {
            return Err("needs at least 1 speaker".into());
        }
        return Ok(Speakers {
            count: n,
            names: Vec::new(),
        });
    }
    let names: Vec<String> = arg.split(',').map(|n| n.trim().to_string()).collect();
    if names.iter().any(|n| n.is_empty()) {
        return Err("expected a number, or names separated by commas (no empty names)".into());
    }
    Ok(Speakers {
        count: names.len(),
        names,
    })
}

impl Cli {
    fn open_options(&self) -> OpenOptions {
        OpenOptions {
            device: self.device.clone(),
            virtual_sink: self.virtual_sink,
            app: self.app.clone(),
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// List input devices for the native audio backend.
    Devices,
    /// List apps whose audio --app can capture.
    Apps,
    /// Download the Zipformer model and the --model recognizer (and the
    /// speaker models with --diarize).
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
    /// --record: the .srt and .ass beside the markdown.
    pub subtitles: Mutex<Vec<SubtitleWriter>>,
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
    /// Length of a file input, for progress.
    pub input_ms: Option<u64>,
    /// --live: a file playing to the speakers rather than a mic.
    pub live: bool,
    /// --diarize: speakers are labelled (and can be renamed with `n`).
    pub diarize: bool,
    /// --record: the audio is being saved too.
    pub recording: bool,
    /// --speakers N.
    pub num_speakers: Option<usize>,
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
        for w in self.subtitles.lock().expect("subtitles lock").iter_mut() {
            w.append_remaining(&t)?;
        }
        Ok(t)
    }

    /// Append a hardened paragraph to the markdown file, if there is one.
    pub fn write(&self, p: &vox_transcribe::Paragraph) -> Result<()> {
        for w in self.subtitles.lock().expect("subtitles lock").iter_mut() {
            w.append(p)?;
        }
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

    /// Start saving markdown to the new file `path`: write `settled` (the
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

/// `-o NAME` writes `NAME.md`; a name already ending in `.md` is kept.
fn markdown_path(name: &Path) -> PathBuf {
    match name.extension().and_then(|e| e.to_str()) {
        Some(e) if e.eq_ignore_ascii_case("md") => name.to_path_buf(),
        _ => {
            let mut s = name.as_os_str().to_owned();
            s.push(".md");
            PathBuf::from(s)
        }
    }
}

/// The files of a `--record NAME` session.
pub struct RecordPaths {
    pub md: PathBuf,
    pub srt: PathBuf,
    pub ass: PathBuf,
    pub opus: PathBuf,
}

impl RecordPaths {
    /// `NAME.md`, `.srt`, `.ass` and `.opus`. One of those extensions on
    /// `base` is dropped, so `notes.opus` and `notes` name the same set.
    pub fn new(base: &Path) -> Self {
        let base = match base.extension().and_then(|e| e.to_str()) {
            Some(e)
                if ["md", "srt", "ass", "opus"]
                    .iter()
                    .any(|x| e.eq_ignore_ascii_case(x)) =>
            {
                base.with_extension("")
            }
            _ => base.to_path_buf(),
        };
        let with = |ext: &str| {
            let mut s = base.clone().into_os_string();
            s.push(ext);
            PathBuf::from(s)
        };
        Self {
            md: with(".md"),
            srt: with(".srt"),
            ass: with(".ass"),
            opus: with(".opus"),
        }
    }

    pub fn all(&self) -> [&PathBuf; 4] {
        [&self.md, &self.srt, &self.ass, &self.opus]
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
    let started = std::time::Instant::now();
    let cli = Cli::parse();
    // A file is transcribed without the TUI, with brief progress on
    // stderr, unless --live plays it through the TUI in real time.
    let tui_mode = cli.cmd.is_none() && !cli.no_tui && (cli.input.is_none() || cli.live);
    let log_file = init_logging(&cli, tui_mode)?;

    match cli.cmd {
        Some(Cmd::Devices) => return list_devices(),
        Some(Cmd::Apps) => return list_apps(),
        Some(Cmd::DownloadModels) => {
            let dir = cli
                .models_dir
                .clone()
                .unwrap_or_else(models::user_models_dir);
            models::download(&dir, cli.model)?;
            if cli.diarize {
                models::download_speakers(&dir)?;
            }
            return Ok(());
        }
        Some(Cmd::StopOnce) => {
            #[cfg(unix)]
            if once::signal_finish() {
                return Ok(());
            }
            eprintln!("no scribe --once recorder is running");
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

    if let Some(name) = &cli.play {
        return play::run(&RecordPaths::new(name));
    }

    let now = chrono::Local::now();
    let record = cli.record.as_deref().map(RecordPaths::new);
    let output = match (&cli.output, &record) {
        (Some(o), _) => Some(markdown_path(o)),
        (None, Some(r)) => Some(r.md.clone()),
        (None, None) => None,
    };
    if cli.append && cli.output.is_none() {
        anyhow::bail!("--append needs -o NAME");
    }
    // Fail before loading models or opening the mic. A recording is
    // never appended to, so only -o mentions --append.
    if let Some(r) = &record {
        if let Some(p) = r.all().into_iter().find(|p| p.exists()) {
            anyhow::bail!("{} already exists; pick another name", p.display());
        }
    } else if let Some(output) = output.as_ref().filter(|o| o.exists() && !cli.append) {
        anyhow::bail!(
            "{} already exists (pass --append to add to it)",
            output.display()
        );
    }
    let corrector = llm_corrector(&cli)?;
    let vocab = match &cli.vocab {
        Some(path) => {
            let v = vox_transcribe::vocab::Vocab::parse(
                &std::fs::read_to_string(path)
                    .with_context(|| format!("read {}", path.display()))?,
            );
            (!v.is_empty()).then_some(v)
        }
        None => None,
    };
    if vocab.is_some() && cli.model == models::Offline::SenseVoice {
        eprintln!("--vocab: SenseVoice can't be biased toward words; only --llm uses the list");
    }

    let llm = corrector.is_some();

    let models_dir = models::resolve(cli.models_dir.as_deref());
    // First run: fetch the models rather than failing.
    if !cli.model.present(&models_dir) || (!cli.no_streaming && !models::has_zipformer(&models_dir))
    {
        eprintln!(
            "models not found, downloading them once ({})…",
            cli.model.download_size()
        );
        models::download(&models_dir, cli.model)?;
    }
    if cli.diarize && !models::has_speakers(&models_dir) {
        eprintln!("speaker models not found, downloading them once (about 77 MB)…");
        models::download_speakers(&models_dir)?;
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
    if cli.input.is_some() && !tui_mode {
        say!("loading models…");
    }

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
            let cap = backend.open(&cli.open_options())?;
            let (rate, name) = (cap.sample_rate, cap.device.clone());
            (Source::Live(cap), rate, name)
        }
    };
    info!(rate, device = %device, "audio source ready");
    let num_speakers = cli.speakers.as_ref().map(|s| s.count);
    if let Some(s) = &cli.speakers {
        speakers::preset(&s.names);
    }
    let speaker_models = cli.diarize.then(|| SpeakerModels {
        num_threads: cli.threads,
        ..SpeakerModels::from_dir(&models_dir.join(models::SPEAKERS))
    });
    let tagger: Option<Box<dyn SpeakerTagger>> = match &speaker_models {
        Some(m) => Some(Box::new(EmbeddingTagger::open(
            m,
            ClusterConfig {
                max_speakers: num_speakers,
                threshold: cli.speaker_threshold,
                ..Default::default()
            },
        )?)),
        None => None,
    };
    // What the final diarization pass reads and rewrites, if it runs:
    // only when there are files to rewrite and the audio is at hand.
    let input_ms = match &source {
        Source::File(s) => Some(s.len() as u64 * 1000 / rate.max(1) as u64),
        Source::Live(_) => None,
    };
    let final_pass = match (&speaker_models, &source, &record) {
        (None, _, _) => None,
        (Some(_), Source::File(s), _) if record.is_some() || output.is_some() => {
            Some(diarize::Audio::Samples(s.clone(), rate))
        }
        (Some(_), _, Some(r)) => Some(diarize::Audio::Opus(r.opus.clone())),
        _ => None,
    };
    let final_pass = if final_pass.is_some() && cli.append {
        eprintln!("--append: the final diarization pass is skipped (live labels only)");
        None
    } else {
        final_pass
    };
    let offline: Arc<dyn vox_transcribe::OfflineRecognizer> = match cli.model {
        models::Offline::SenseVoice => {
            let mut sv = SenseVoiceConfig::from_dir(&models_dir.join(models::SENSE_VOICE));
            sv.language = cli.language.clone();
            sv.num_threads = cli.threads;
            Arc::new(
                SenseVoice::open(&sv)
                    .with_context(|| format!("loading SenseVoice from {}", models_dir.display()))?,
            )
        }
        models::Offline::Parakeet => {
            let mut pc = ParakeetConfig::from_dir(&models_dir.join(models::PARAKEET));
            pc.num_threads = cli.threads;
            let plain: Arc<dyn vox_transcribe::OfflineRecognizer> = Arc::new(
                Parakeet::open(&pc)
                    .with_context(|| format!("loading Parakeet from {}", models_dir.display()))?,
            );
            match &vocab {
                Some(v) => {
                    let biased = Parakeet::open_biased(
                        &pc,
                        &v.terms,
                        vox_transcribe::vocab::HOTWORD_SCORE,
                        vox_transcribe::vocab::BEAM,
                    )?;
                    Arc::new(vox_transcribe::vocab::VocabRecognizer {
                        biased: Arc::new(biased),
                        plain,
                        vocab: v.clone(),
                        min_similarity: vox_transcribe::vocab::MIN_SIMILARITY,
                    })
                }
                None => plain,
            }
        }
    };
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
    let writer = match output.clone() {
        Some(o) => Some(MarkdownWriter::create(o, &title, &subtitle, cli.append)?),
        None => None,
    };
    let (subtitles, recording) = match &record {
        Some(r) => (
            vec![
                SubtitleWriter::create(r.srt.clone(), Format::Srt, false)?,
                SubtitleWriter::create(r.ass.clone(), Format::Ass, false)?,
            ],
            Some(OpusWriter::create(&r.opus, rate)?),
        ),
        None => (Vec::new(), None),
    };

    let manual = cli.manual || cli.once;
    let mut engine_cfg = EngineConfig::new(rate);
    if manual {
        engine_cfg.paragraph.mode = ParagraphMode::Manual;
    }
    let engine = Engine::spawn_full(engine_cfg, streaming, offline, corrector, tagger);
    let loaded = started.elapsed();
    let fed = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let paused = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let source_done = Arc::new(AtomicBool::new(false));

    let (capture, pump) = {
        let mut feed = Feed {
            pusher: engine.pusher(),
            recording,
            fed: fed.clone(),
        };
        let (paused, stop, done) = (paused.clone(), stop.clone(), source_done.clone());
        match source {
            Source::Live(cap) => {
                let capture = Arc::new(Mutex::new(Some(cap)));
                let live = LivePump {
                    capture: capture.clone(),
                    opts: cli.open_options(),
                    rate,
                    feed,
                    paused,
                    stop,
                };
                (capture, std::thread::spawn(move || live.run()))
            }
            Source::File(samples) if cli.live => {
                let out = vox_audio::playback::Output::open().context("open audio output")?;
                info!(device = %out.device, rate = out.sample_rate, "playing the input live");
                let player = LivePlayer {
                    samples,
                    rate,
                    out,
                    feed,
                    paused,
                    stop,
                };
                let pump = std::thread::spawn(move || {
                    player.run();
                    done.store(true, Ordering::SeqCst);
                });
                (Arc::new(Mutex::new(None)), pump)
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
                        feed.push(chunk);
                    }
                    feed.finish();
                    done.store(true, Ordering::SeqCst);
                });
                (Arc::new(Mutex::new(None)), pump)
            }
        }
    };

    let files = diarize::Files {
        md: output.clone(),
        srt: record.as_ref().map(|r| r.srt.clone()),
        ass: record.as_ref().map(|r| r.ass.clone()),
        title: title.clone(),
        subtitle: subtitle.clone(),
    };
    let session = Session {
        engine,
        writer: Mutex::new(writer),
        subtitles: Mutex::new(subtitles),
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
        input_ms,
        live: cli.live,
        diarize: cli.diarize,
        recording: record.is_some(),
        num_speakers,
        pump: Some(pump),
    };
    let mut outcome = match screen {
        Some(screen) => tui::run(screen, session)?,
        None => headless(session)?,
    };
    let transcribed = started.elapsed();
    let mut diarized = None;
    let mut rewritten = false;
    if let (Some(audio), Some(models)) = (final_pass, speaker_models) {
        let has_text = outcome
            .transcript
            .paragraphs
            .iter()
            .any(|p| !p.text.trim().is_empty());
        if has_text {
            let t0 = std::time::Instant::now();
            let result = diarize::run(
                models,
                num_speakers,
                audio,
                &outcome.transcript,
                &files,
                tui_mode,
            );
            if !matches!(result, Ok(None)) {
                diarized = Some(t0.elapsed());
            }
            match result {
                Ok(Some(t)) => {
                    let mut who: Vec<&str> = t
                        .paragraphs
                        .iter()
                        .filter_map(|p| p.speaker.as_deref())
                        .collect();
                    who.sort_unstable();
                    who.dedup();
                    say!("diarized: {} speaker(s)", who.len());
                    outcome.transcript = t;
                    rewritten = true;
                }
                Ok(None) => say!("diarization skipped; files keep the live speaker labels"),
                Err(e) => {
                    say!("diarization failed, files keep the live speaker labels: {e:#}")
                }
            }
        }
    }
    // Speakers renamed during the session: the files were written with
    // the old names as they went, so write them again.
    if speakers::renamed() && !rewritten && !cli.append {
        let files = diarize::Files {
            md: outcome.path.clone(),
            ..files
        };
        if let Err(e) = diarize::rewrite(&files, &outcome.transcript) {
            say!("rewriting the files with the new speaker names failed: {e:#}");
        }
    }
    if cli.once {
        copy_transcript(&outcome);
    }
    if let Some(path) = &outcome.path {
        say!("saved {}", path.display());
    }
    if let Some(r) = &record {
        for p in [&r.srt, &r.ass, &r.opus] {
            say!("saved {}", p.display());
        }
        say!(
            "play it back with: scribe --play {}",
            r.md.with_extension("").display()
        );
    }
    if !cli.once {
        let audio_ms = fed.load(Ordering::Relaxed) * 1000 / rate.max(1) as u64;
        let offline = cli.input.is_some() && !cli.live;
        let stats = Stats {
            audio_ms,
            total: started.elapsed(),
            loading: loaded,
            session: transcribed.saturating_sub(loaded),
            diarization: diarized,
            offline,
        };
        for line in stats.lines() {
            say!("{line}");
        }
    }
    Ok(())
}

/// Timings shown when a session ends.
struct Stats {
    audio_ms: u64,
    total: Duration,
    loading: Duration,
    /// Transcription (a file) or the session itself (live).
    session: Duration,
    diarization: Option<Duration>,
    /// A file transcribed as fast as possible, not paced like a mic.
    offline: bool,
}

impl Stats {
    fn lines(&self) -> Vec<String> {
        let clock = |d: Duration| progress::clock(d.as_millis() as u64);
        let audio = progress::clock(self.audio_ms);
        let mut out = Vec::new();
        if self.audio_ms == 0 {
            return out;
        }
        if self.offline {
            out.push(format!(
                "done: {audio} of audio in {} ({})",
                clock(self.total),
                speed(self.audio_ms, self.total)
            ));
        } else {
            out.push(format!(
                "done: {audio} of audio, {} in all",
                clock(self.total)
            ));
        }
        let mut parts = vec![format!("loading {}", clock(self.loading))];
        parts.push(if self.offline {
            format!(
                "transcription {} ({})",
                clock(self.session),
                speed(self.audio_ms, self.session)
            )
        } else {
            format!("recording {}", clock(self.session))
        });
        if let Some(d) = self.diarization {
            parts.push(format!(
                "diarization {} ({})",
                clock(d),
                speed(self.audio_ms, d)
            ));
        }
        out.push(format!("  {}", parts.join(" · ")));
        out
    }
}

/// "2.3x faster than real time" / "1.4x slower than real time" for
/// `audio_ms` of audio processed in `took`.
fn speed(audio_ms: u64, took: Duration) -> String {
    let took_ms = took.as_millis().max(1) as f64;
    let ratio = audio_ms as f64 / took_ms;
    if ratio >= 1.0 {
        format!("{ratio:.1}x faster than real time")
    } else {
        format!("{:.1}x slower than real time", 1.0 / ratio.max(1e-9))
    }
}

/// --once: print the finished text and put it on the clipboard.
fn copy_transcript(outcome: &Outcome) {
    if !outcome.confirmed {
        say!("cancelled, nothing copied");
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
        say!("nothing transcribed, clipboard unchanged");
        return;
    }
    println!("{text}");
    let words = text.split_whitespace().count();
    match clipboard::copy(&text) {
        Ok(how) => say!("copied {words} words ({how})"),
        Err(e) => say!("copy failed: {e}"),
    }
}

/// Feeds mic audio to the engine. While paused it releases the mic (so
/// the OS stops showing it in use) and pushes silence on wall-clock time,
/// keeping timestamps aligned; on resume it reopens the device.
/// Where captured audio goes: the engine, and the --record file.
struct Feed {
    pusher: vox_transcribe::Pusher,
    recording: Option<OpusWriter>,
    /// Samples pushed, for the end-of-run stats.
    fed: Arc<std::sync::atomic::AtomicU64>,
}

impl Feed {
    fn push(&mut self, mono: &[f32]) {
        self.fed.fetch_add(mono.len() as u64, Ordering::Relaxed);
        self.pusher.push(mono);
        if let Some(r) = &mut self.recording {
            if let Err(e) = r.write(mono) {
                tracing::error!("recording stopped: {e:#}");
                self.recording = None;
            }
        }
    }

    fn finish(self) {
        if let Some(r) = self.recording {
            if let Err(e) = r.finish() {
                tracing::error!("finishing the recording failed: {e:#}");
            }
        }
    }
}

/// --live: plays a file to the speakers and feeds the engine each sample
/// as it is heard, so transcription runs in real time like a mic. While
/// paused, playback and feeding both stop, so times stay in file time.
struct LivePlayer {
    samples: Vec<f32>,
    rate: u32,
    out: vox_audio::playback::Output,
    feed: Feed,
    paused: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl LivePlayer {
    fn run(mut self) {
        let len = self.samples.len();
        let out_rate = self.out.sample_rate.max(1) as u64;
        let mut resampler = vox_audio::resample::Linear::new(self.rate, self.out.sample_rate);
        let chunk = (self.rate as usize / 50).max(1);
        // Input samples handed to the output, and to the engine.
        let (mut queued, mut fed) = (0usize, 0usize);
        let mut pending: Vec<f32> = Vec::new();
        let mut was_paused = false;
        while !self.stop.load(Ordering::Relaxed) {
            let paused = self.paused.load(Ordering::Relaxed);
            if paused != was_paused {
                self.out.set_paused(paused);
                was_paused = paused;
            }
            // Keep the output ring topped up.
            while self.out.free() > 0 {
                if pending.is_empty() {
                    if queued >= len {
                        break;
                    }
                    let end = (queued + chunk).min(len);
                    resampler.process(&self.samples[queued..end], &mut pending);
                    queued = end;
                }
                let n = self.out.push(&pending);
                pending.drain(..n);
                if n == 0 {
                    break;
                }
            }
            // Feed the engine what has been played. Once everything has
            // drained, the resampler's tail rounding no longer matters.
            let drained = queued >= len && pending.is_empty() && self.out.queued() == 0;
            let heard = if drained {
                len
            } else {
                ((self.out.played() * self.rate as u64 / out_rate) as usize).min(len)
            };
            if heard > fed {
                self.feed.push(&self.samples[fed..heard]);
                fed = heard;
            }
            if fed >= len {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // No silence follows the end of a file to close its last
        // utterance, so close it now rather than at quit.
        if fed >= len {
            self.feed.pusher.break_paragraph();
        }
        self.feed.finish();
    }
}

struct LivePump {
    capture: Arc<Mutex<Option<Capture>>>,
    opts: OpenOptions,
    rate: u32,
    feed: Feed,
    paused: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl LivePump {
    fn run(mut self) {
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
                        self.feed.push(&vec![0.0; chunk.len()]);
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
                self.feed.push(&chunk);
            }
        }
        self.feed.finish();
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
    fn catch_up(&mut self, since: std::time::Instant, pushed: &mut u64) {
        let due = (since.elapsed().as_secs_f64() * self.rate as f64) as u64;
        if due > *pushed {
            self.feed.push(&vec![0.0; (due - *pushed) as usize]);
            *pushed = due;
        }
    }
}

/// Headless mode: Ctrl-C quits at once rather than asking the session to
/// stop (see [`headless`]).
pub static CTRL_C_QUITS: AtomicBool = AtomicBool::new(false);

fn headless(session: Session) -> Result<Outcome> {
    let interrupted = Arc::new(AtomicBool::new(false));
    {
        let i = interrupted.clone();
        ctrlc::set_handler(move || {
            // The first press ends a mic session gracefully; a second one,
            // or any press when there is nothing graceful to do (a file
            // input, diarizing), quits.
            if CTRL_C_QUITS.load(Ordering::SeqCst) || i.swap(true, Ordering::SeqCst) {
                say!("interrupted; files keep what was saved so far");
                std::process::exit(130);
            }
        })
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
    // A file has nothing to stop gracefully (it is all queued at once),
    // so Ctrl-C just quits.
    if session.input_ms.is_some() {
        CTRL_C_QUITS.store(true, Ordering::SeqCst);
    }
    if let Some(ms) = session.input_ms {
        say!(
            "transcribing {} ({} of audio)…",
            session.device,
            timestamp(ms)
        );
    }
    // A file transcribed into files keeps stdout quiet; otherwise the
    // paragraphs are the output.
    let has_files =
        session.path().is_some() || !session.subtitles.lock().expect("subtitles lock").is_empty();
    let into_files = session.input_ms.is_some() && has_files;
    let to_stdout = !session.once && !into_files;
    let mut progress = progress::Progress::new(session.input_ms);
    let mut printed = HashSet::new();
    let print = |p: &vox_transcribe::Paragraph, printed: &mut HashSet<String>| {
        if !p.text.trim().is_empty() && printed.insert(p.id.clone()) {
            let who = p
                .speaker
                .as_deref()
                .map(|s| format!(" {}:", speakers::name(s)))
                .unwrap_or_default();
            println!("[{}]{who} {}\n", timestamp(p.start_ms), p.text.trim());
        }
    };
    while !interrupted.load(Ordering::SeqCst)
        && !entered.load(Ordering::SeqCst)
        && !session.finish_requested.load(Ordering::SeqCst)
        && !session.source_done.load(Ordering::SeqCst)
    {
        if let Ok(Event::Paragraph { paragraph, change }) =
            events.recv_timeout(Duration::from_millis(100))
        {
            progress.update(&paragraph);
            if change == Change::Hardened {
                if to_stdout {
                    progress.clear();
                    print(&paragraph, &mut printed);
                }
                session.write(&paragraph)?;
            }
        }
        progress.tick();
    }
    let confirmed = !interrupted.load(Ordering::SeqCst);
    let path = session.path();
    let input = session.input_ms.is_some();
    // A file is fed faster than pass 2 keeps up, so most of the work is
    // still queued here; keep reporting progress while it drains.
    let t = std::thread::scope(|sc| {
        let finishing = sc.spawn(move || session.finish());
        while !finishing.is_finished() {
            if let Ok(Event::Paragraph { paragraph, .. }) =
                events.recv_timeout(Duration::from_millis(100))
            {
                progress.update(&paragraph);
            }
            progress.tick();
        }
        progress.clear();
        finishing
            .join()
            .unwrap_or_else(|_| Err(anyhow::anyhow!("finishing the session panicked")))
    })?;
    if to_stdout {
        for p in &t.paragraphs {
            print(p, &mut printed);
        }
    }
    if input {
        let n = t
            .paragraphs
            .iter()
            .filter(|p| !p.text.trim().is_empty())
            .count();
        say!("transcribed {n} paragraph(s)");
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

fn list_apps() -> Result<()> {
    let backend = vox_audio::default_backend()?;
    let mut apps = backend.list_apps()?;
    apps.sort_by_key(|a| (!a.playing, a.name.to_lowercase()));
    println!("{} apps with audio (* = playing now):", backend.name());
    for a in apps {
        let mark = if a.playing { "*" } else { " " };
        println!("{mark} {:>7}  {}  {}", a.pid, a.name, a.id);
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

/// Returns the log file in TUI mode, for redirecting fd 2 into it.
fn init_logging(cli: &Cli, tui_mode: bool) -> Result<Option<std::fs::File>> {
    use tracing_subscriber::EnvFilter;
    // A file input prints its own brief progress; keep logs to warnings.
    let quiet = cli.input.is_some() && !cli.live && cli.log.is_none();
    let filter = EnvFilter::try_from_env("VOX_SCRIBE_LOG").unwrap_or_else(|_| {
        EnvFilter::new(if quiet {
            "warn"
        } else {
            "warn,scribe=info,vox_transcribe=info,vox_audio=info"
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speakers_take_a_count_or_names() {
        assert_eq!(
            parse_speakers("3"),
            Ok(Speakers {
                count: 3,
                names: vec![]
            })
        );
        assert_eq!(
            parse_speakers(" Alice, Bob ,Carol "),
            Ok(Speakers {
                count: 3,
                names: vec!["Alice".into(), "Bob".into(), "Carol".into()]
            })
        );
        assert_eq!(parse_speakers("Alice").map(|s| s.count), Ok(1));
        assert!(parse_speakers("0").is_err());
        assert!(parse_speakers("Alice,,Bob").is_err());
        assert!(parse_speakers("").is_err());
    }

    #[test]
    fn speed_reads_both_ways() {
        assert_eq!(
            speed(244_000, Duration::from_secs(122)),
            "2.0x faster than real time"
        );
        assert_eq!(
            speed(60_000, Duration::from_secs(90)),
            "1.5x slower than real time"
        );
    }
}
