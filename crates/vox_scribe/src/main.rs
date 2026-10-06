//! vox_scribe: live transcription in the terminal, saved as markdown.
//!
//! Uses vox_transcribe's three passes: streaming Zipformer partials
//! (ALL CAPS, instant), Parakeet (or SenseVoice) re-decodes of each
//! utterance, and boundary re-transcription across neighbouring
//! utterances. With --diarize each utterance is also labelled with its
//! speaker, and a full diarization relabels the saved files at the end.
//! Alternatively, several sources (-d / -a / -i, repeated) are each
//! transcribed on their own as one speaker apiece.

mod chat;
mod chatdb;
mod clipboard;
mod diarize;
mod markdown;
mod models;
#[cfg(unix)]
mod once;
mod picker;
mod play;
mod progress;
mod sources;
mod speak;
mod speakers;
mod subtitles;
mod tui;
mod voice;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result};
use clap::{CommandFactory as _, FromArgMatches as _, Parser, Subcommand};
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
    /// Repeat it (and/or -a) to transcribe several sources at once, each
    /// on its own as one speaker: Speaker A, B, … in the order given, or
    /// the names from --speakers.
    #[arg(short, long)]
    device: Vec<String>,
    /// PipeWire only: register a virtual sink named `vox_scribe` and
    /// transcribe whatever is played into it.
    #[arg(long)]
    virtual_sink: bool,
    /// Transcribe what one app is playing instead of an input: a PID or a
    /// name / bundle id substring (see `apps`). The app still plays to
    /// your speakers. macOS 14.4+ (process tap) or PipeWire. Repeatable,
    /// and combines with -d: one speaker per source.
    #[arg(short, long, conflicts_with_all = ["virtual_sink", "input"])]
    app: Vec<String>,
    /// Transcribe an audio file (wav, flac, mp3, ogg) instead of a device.
    /// Repeat it for per-speaker tracks of one session (one speaker per
    /// file, all starting at the same moment).
    #[arg(short, long, conflicts_with_all = ["device", "virtual_sink"])]
    input: Vec<PathBuf>,
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
    /// shown word by word in time with the audio. With no NAME.opus,
    /// NAME.md is read aloud by a local voice model (Kokoro) instead.
    #[arg(short, long, conflicts_with_all = ["output", "record", "input", "app", "device", "virtual_sink", "once", "no_tui"])]
    play: Option<PathBuf>,
    /// With --play reading a transcript aloud: Kokoro voices for the
    /// speakers in order of appearance, e.g. `am_adam,bf_emma`. The rest
    /// get the built-in cast (af_heart, am_michael, bf_emma, bm_george, …).
    /// With --chat, the first one is the voice of the replies.
    #[arg(long, value_delimiter = ',')]
    voices: Vec<String>,
    /// Talk with a responder through NAME.db (`.db` is added if missing):
    /// Enter sends what you said, the reply is read aloud. See
    /// `scribe chat-echo` for a stand-in responder.
    #[arg(long, conflicts_with_all = ["output", "record", "play", "input", "once", "no_tui", "append", "live"])]
    chat: Option<PathBuf>,
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
    /// voice goes to one of them. With several sources: their names, in
    /// the order the sources were given. Rename speakers any time with
    /// `n` in the TUI.
    #[arg(long, value_parser = parse_speakers)]
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

/// What to capture for each live source, in command-line order (-d and
/// -a interleaved as typed). One default source when none is given.
fn live_sources(cli: &Cli, matches: &clap::ArgMatches) -> Vec<OpenOptions> {
    let at = |id: &str| -> Vec<usize> {
        matches
            .indices_of(id)
            .map(|i| i.collect())
            .unwrap_or_default()
    };
    let mut list: Vec<(usize, OpenOptions)> = Vec::new();
    for (i, d) in at("device").into_iter().zip(&cli.device) {
        list.push((
            i,
            OpenOptions {
                device: Some(d.clone()),
                ..Default::default()
            },
        ));
    }
    for (i, a) in at("app").into_iter().zip(&cli.app) {
        list.push((
            i,
            OpenOptions {
                app: Some(a.clone()),
                ..Default::default()
            },
        ));
    }
    list.sort_by_key(|(i, _)| *i);
    if let [(_, only)] = list.as_mut_slice() {
        only.virtual_sink = cli.virtual_sink;
    }
    if list.is_empty() {
        return vec![OpenOptions {
            virtual_sink: cli.virtual_sink,
            ..Default::default()
        }];
    }
    list.into_iter().map(|(_, o)| o).collect()
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
    /// A stand-in --chat responder: reads back whatever is said in
    /// NAME.db, a sentence at a time, then asks for more.
    ChatEcho { name: PathBuf },
    /// Finish the running --once recorder (as if Enter were pressed).
    /// Exits 1 if none is running.
    StopOnce,
}

/// Audio source feeding an engine.
enum Source {
    Live(Capture, OpenOptions),
    File(Vec<f32>),
}

/// Everything the UI loops need.
pub struct Session {
    /// One engine per source.
    pub engine: sources::Engines,
    /// None until -o or the TUI's `s` names a file.
    pub writer: Mutex<Option<MarkdownWriter>>,
    /// --record: the .srt and .ass beside the markdown.
    pub subtitles: Mutex<Vec<SubtitleWriter>>,
    /// Heading and date line for a file started with `s`.
    pub title: String,
    pub subtitle: String,
    pub paused: Arc<AtomicBool>,
    /// The mic stays open but is heard as silence (--chat, while a
    /// reply is being spoken).
    pub muted: Arc<AtomicBool>,
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
    /// One per engine, in order: what each source captures.
    pub slots: Mutex<Vec<Arc<Slot>>>,
    /// Live input: starts engines for sources added with `a`.
    spawner: Option<Spawner>,
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
    /// --speakers N, or the number of sources.
    pub num_speakers: Option<usize>,
    pumps: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

/// One source feeding one engine. A live slot's input can be swapped or
/// switched off from the TUI (`a`); the engine, and so the speaker
/// label, stays.
pub struct Slot {
    pub opts: Mutex<OpenOptions>,
    /// Deselected: the input is released and the engine fed silence.
    pub off: AtomicBool,
    /// `opts` changed: reopen with them.
    reopen: AtomicBool,
    /// The live input; None for a file, or while paused or off.
    pub capture: Mutex<Option<Capture>>,
    pub name: Mutex<String>,
    /// A problem to show in the TUI once: a failed switch, or an app
    /// that plays but records as silence.
    pub notice: Mutex<Option<String>>,
    pub live: bool,
    rate: u32,
    /// Samples fed so far.
    fed: Arc<std::sync::atomic::AtomicU64>,
}

impl Slot {
    fn new(opts: OpenOptions, capture: Option<Capture>, rate: u32, name: String) -> Self {
        Self {
            live: capture.is_some(),
            opts: Mutex::new(opts),
            off: AtomicBool::new(false),
            reopen: AtomicBool::new(false),
            capture: Mutex::new(capture),
            name: Mutex::new(name),
            notice: Mutex::new(None),
            rate,
            fed: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    fn fed_ms(&self) -> u64 {
        self.fed.load(Ordering::Relaxed) * 1000 / self.rate.max(1) as u64
    }
}

/// What a source added mid-session needs for its engine.
struct Spawner {
    offline: Arc<dyn vox_transcribe::OfflineRecognizer>,
    zipformer: Option<Zipformer>,
    corrector: Option<Arc<dyn Corrector>>,
    /// --record: the mix every live source goes into.
    mixer: Option<Arc<Mutex<sources::Mixer>>>,
}

fn new_engine(
    rate: u32,
    manual: bool,
    chat: bool,
    zipformer: Option<&Zipformer>,
    offline: &Arc<dyn vox_transcribe::OfflineRecognizer>,
    corrector: &Option<Arc<dyn Corrector>>,
    tagger: Option<Box<dyn SpeakerTagger>>,
) -> Engine {
    let mut cfg = EngineConfig::new(rate);
    if manual {
        cfg.paragraph.mode = ParagraphMode::Manual;
    }
    if chat {
        // What was said is sent once it hardens: soon after Enter, and
        // as one message however long it runs.
        cfg.paragraph.gap_ms = 300;
        cfg.paragraph.soft_max_words = usize::MAX;
        cfg.paragraph.hard_max_words = usize::MAX;
    }
    let streaming = zipformer.map(|z| Box::new(z.session()) as Box<dyn StreamingRecognizer>);
    Engine::spawn_full(cfg, streaming, offline.clone(), corrector.clone(), tagger)
}

impl Session {
    /// Several sources, each its own speaker (also renamed with `n`).
    pub fn per_source(&self) -> bool {
        self.engine.labelled().load(Ordering::Relaxed)
    }

    /// Speakers to offer in `n` before any is heard.
    pub fn speaker_count(&self) -> usize {
        let sources = if self.per_source() {
            self.engine.len()
        } else {
            0
        };
        sources.max(self.num_speakers.unwrap_or(0))
    }

    /// What is being transcribed, for the header.
    pub fn device(&self) -> String {
        self.slots
            .lock()
            .expect("slots lock")
            .iter()
            .filter(|s| !s.off.load(Ordering::Relaxed))
            .map(|s| s.name.lock().expect("name lock").clone())
            .collect::<Vec<_>>()
            .join(" + ")
    }

    /// Sources can be changed (`a`): the default input, with no -d / -a /
    /// -i / --virtual-sink on the command line.
    pub fn can_switch(&self) -> bool {
        self.spawner.is_some()
    }

    /// Listen to slots `keep` plus `new` inputs from now on; the other
    /// slots go silent. A new input takes over a silent slot (and its
    /// speaker label) when there is one, else gets its own engine.
    pub fn set_sources(&self, keep: &[usize], new: Vec<OpenOptions>) -> Result<()> {
        let Some(sp) = &self.spawner else {
            anyhow::bail!("sources were set on the command line");
        };
        let n = keep.len() + new.len();
        if n == 0 {
            anyhow::bail!("pick at least one source");
        }
        if self.diarize && n > 1 {
            anyhow::bail!("--diarize listens to one source");
        }
        let mut slots = self.slots.lock().expect("slots lock");
        let mut free: std::collections::VecDeque<usize> =
            (0..slots.len()).filter(|i| !keep.contains(i)).collect();
        for opts in new {
            match free.pop_front() {
                Some(i) => {
                    let s = &slots[i];
                    *s.opts.lock().expect("opts lock") = opts;
                    s.reopen.store(true, Ordering::SeqCst);
                    s.off.store(false, Ordering::SeqCst);
                }
                None => {
                    let slot = self.add_source(sp, &slots, opts)?;
                    slots.push(slot);
                }
            }
        }
        for i in free {
            slots[i].off.store(true, Ordering::SeqCst);
        }
        for &i in keep {
            slots[i].off.store(false, Ordering::SeqCst);
        }
        Ok(())
    }

    /// Open `opts` with an engine of its own, starting at the session's
    /// current time.
    fn add_source(
        &self,
        sp: &Spawner,
        slots: &[Arc<Slot>],
        opts: OpenOptions,
    ) -> Result<Arc<Slot>> {
        let cap = vox_audio::default_backend()?.open(&opts)?;
        let rate = cap.sample_rate;
        let now_ms = slots.iter().map(|s| s.fed_ms()).max().unwrap_or(0);
        let engine = new_engine(
            rate,
            self.manual.load(Ordering::Relaxed),
            false,
            sp.zipformer.as_ref(),
            &sp.offline,
            &sp.corrector,
            None,
        );
        let pusher = engine.pusher();
        self.engine.add(engine);
        let recording = sp.mixer.as_ref().map(|m| {
            let track = m.lock().expect("mixer lock").add_track(rate);
            Recorder::Mixed(m.clone(), track)
        });
        let name = cap.device.clone();
        info!(rate, device = %name, "audio source added");
        let slot = Arc::new(Slot::new(opts, Some(cap), rate, name));
        let lead = now_ms * rate as u64 / 1000;
        slot.fed.store(lead, Ordering::SeqCst);
        let pump = LivePump {
            slot: slot.clone(),
            rate,
            feed: Feed {
                pusher,
                recording,
                fed: slot.fed.clone(),
            },
            paused: self.paused.clone(),
            muted: self.muted.clone(),
            stop: self.stop.clone(),
            lead,
        };
        self.pumps
            .lock()
            .expect("pumps lock")
            .push(std::thread::spawn(move || pump.run()));
        Ok(slot)
    }

    /// Stop the audio, drain every pass, append what's left to the markdown.
    pub fn finish(self) -> Result<Transcript> {
        self.stop.store(true, Ordering::SeqCst);
        for s in self.slots.lock().expect("slots lock").iter() {
            s.capture.lock().expect("capture lock").take();
        }
        let pumps = std::mem::take(&mut *self.pumps.lock().expect("pumps lock"));
        for p in pumps {
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

/// What the model-loading thread needs, owned so it can run while the
/// TUI is already up.
struct Load {
    model: models::Offline,
    language: String,
    threads: i32,
    streaming: bool,
    models_dir: PathBuf,
    vocab: Option<vox_transcribe::vocab::Vocab>,
    speakers: Option<SpeakerModels>,
    cluster: ClusterConfig,
}

struct Loaded {
    offline: Arc<dyn vox_transcribe::OfflineRecognizer>,
    zipformer: Option<Zipformer>,
    tagger: Option<Box<dyn SpeakerTagger>>,
}

impl Load {
    fn run(self) -> Result<Loaded> {
        let models_dir = &self.models_dir;
        let tagger: Option<Box<dyn SpeakerTagger>> = match &self.speakers {
            Some(m) => Some(Box::new(EmbeddingTagger::open(m, self.cluster)?)),
            None => None,
        };
        let offline: Arc<dyn vox_transcribe::OfflineRecognizer> =
            match self.model {
                models::Offline::SenseVoice => {
                    let mut sv = SenseVoiceConfig::from_dir(&models_dir.join(models::SENSE_VOICE));
                    sv.language = self.language;
                    sv.num_threads = self.threads;
                    Arc::new(SenseVoice::open(&sv).with_context(|| {
                        format!("loading SenseVoice from {}", models_dir.display())
                    })?)
                }
                models::Offline::Parakeet => {
                    let mut pc = ParakeetConfig::from_dir(&models_dir.join(models::PARAKEET));
                    pc.num_threads = self.threads;
                    let plain: Arc<dyn vox_transcribe::OfflineRecognizer> =
                        Arc::new(Parakeet::open(&pc).with_context(|| {
                            format!("loading Parakeet from {}", models_dir.display())
                        })?);
                    match &self.vocab {
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
        // One streaming model, a session (stream) per source.
        let zipformer = if self.streaming {
            let mut zc = ZipformerConfig::from_dir(&models_dir.join(models::ZIPFORMER));
            zc.num_threads = self.threads;
            Some(Zipformer::open(&zc)?)
        } else {
            None
        };
        Ok(Loaded {
            offline,
            zipformer,
            tagger,
        })
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
    let matches = Cli::command().get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    // A file is transcribed without the TUI, with brief progress on
    // stderr, unless --live plays it through the TUI in real time.
    let tui_mode = cli.cmd.is_none() && !cli.no_tui && (cli.input.is_empty() || cli.live);
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
        Some(Cmd::ChatEcho { name }) => return chatdb::echo(&chatdb::path_for(&name)),
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
        let paths = RecordPaths::new(name);
        if paths.opus.exists() || !paths.md.exists() {
            return play::run(&paths);
        }
        let models_dir = models::resolve(cli.models_dir.as_deref());
        if !models::has_kokoro(&models_dir) {
            eprintln!(
                "no {} to play; reading {} aloud instead",
                paths.opus.display(),
                paths.md.display()
            );
            eprintln!("voice model not found, downloading it once (about 350 MB)…");
            models::download_kokoro(&models_dir)?;
        }
        // sherpa-onnx logs to fd 2 while loading; keep it off the TUI.
        #[cfg(unix)]
        let _stderr_restore = match &log_file {
            Some(f) => Some(vox_transcribe::stderr::redirect_to(f)?),
            None => None,
        };
        return play::run_speech(&paths, &models::kokoro_dir(&models_dir), &cli.voices);
    }

    let live_opts = live_sources(&cli, &matches);
    let n_sources = if cli.input.is_empty() {
        live_opts.len()
    } else {
        cli.input.len()
    };
    let per_source = n_sources > 1;
    if per_source {
        if cli.diarize {
            anyhow::bail!(
                "--diarize tells speakers apart within one source; with several sources each one is its own speaker already"
            );
        }
        if cli.virtual_sink {
            anyhow::bail!(
                "--virtual-sink is one source; it can't be combined with several -d / -a"
            );
        }
        if cli.live {
            anyhow::bail!("--live plays one file; drop it to transcribe several -i files");
        }
    }
    if let Some(s) = &cli.speakers {
        if per_source {
            if s.count > n_sources {
                anyhow::bail!(
                    "--speakers names {} speakers for {n_sources} sources",
                    s.count
                );
            }
        } else if !cli.diarize {
            anyhow::bail!("--speakers needs --diarize, or several sources (-d / -a / -i repeated)");
        }
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
    if cli.chat.is_some() && !models::has_kokoro(&models_dir) {
        eprintln!("voice model not found, downloading it once (about 350 MB)…");
        models::download_kokoro(&models_dir)?;
    }

    // Put the TUI on screen now so the mic and models load behind it
    // rather than behind a blank terminal. sherpa-onnx's C++ code writes to
    // fd 2 directly; send it to the log while the TUI owns the screen.
    #[cfg(unix)]
    let _stderr_restore = match (&log_file, tui_mode) {
        (Some(f), true) => Some(vox_transcribe::stderr::redirect_to(f)?),
        _ => None,
    };
    let view = |device| tui::Loading {
        device,
        recording: record.is_some(),
        once: cli.once,
        saving: output.is_some(),
    };
    let mut screen = tui_mode.then(|| tui::Screen::splash(&view("")));
    if !cli.input.is_empty() && !tui_mode {
        say!("loading models…");
    }

    // Open the mic before loading models so speech during the load is
    // buffered (the capture queue holds ~10 s) rather than lost.
    // (source, its sample rate, its name), in speaker order.
    let mut srcs: Vec<(Source, u32, String)> = Vec::new();
    if cli.input.is_empty() {
        let backend = vox_audio::default_backend()?;
        for opts in live_opts {
            let cap = backend
                .open(&opts)
                .with_context(|| match (&opts.device, &opts.app) {
                    (Some(d), _) => format!("open device {d}"),
                    (_, Some(a)) => format!("capture app {a}"),
                    _ => "open the default input".into(),
                })?;
            let (rate, name) = (cap.sample_rate, cap.device.clone());
            srcs.push((Source::Live(cap, opts), rate, name));
        }
    } else {
        for path in &cli.input {
            let d = vox_audio::file::decode(path)?;
            srcs.push((
                Source::File(d.samples),
                d.sample_rate,
                path.display().to_string(),
            ));
        }
    }
    for (i, (_, rate, name)) in srcs.iter().enumerate() {
        info!(source = i, rate, device = %name, "audio source ready");
    }
    let device = srcs
        .iter()
        .map(|(_, _, name)| name.as_str())
        .collect::<Vec<_>>()
        .join(" + ");
    let num_speakers = if per_source {
        Some(n_sources)
    } else {
        cli.speakers.as_ref().map(|s| s.count)
    };
    if let Some(s) = &cli.speakers {
        speakers::preset(&s.names);
    }
    let speaker_models = cli.diarize.then(|| SpeakerModels {
        num_threads: cli.threads,
        ..SpeakerModels::from_dir(&models_dir.join(models::SPEAKERS))
    });
    // Load the models on a thread so the TUI shows the transcript view
    // (and the mic keeps buffering) while they come up.
    let load = {
        let load = Load {
            model: cli.model,
            language: cli.language.clone(),
            threads: cli.threads,
            streaming: !cli.no_streaming && models::has_zipformer(&models_dir),
            models_dir: models_dir.clone(),
            vocab: vocab.clone(),
            speakers: speaker_models.clone(),
            cluster: ClusterConfig {
                max_speakers: num_speakers,
                threshold: cli.speaker_threshold,
                ..Default::default()
            },
        };
        std::thread::spawn(move || load.run())
    };
    // What the final diarization pass reads and rewrites, if it runs:
    // only when there are files to rewrite and the audio is at hand.
    let input_ms = srcs
        .iter()
        .filter_map(|(src, rate, _)| match src {
            Source::File(s) => Some(s.len() as u64 * 1000 / (*rate).max(1) as u64),
            Source::Live(..) => None,
        })
        .max();
    // --diarize is single-source.
    let (first, first_rate) = (&srcs[0].0, srcs[0].1);
    let final_pass = match (&speaker_models, first, &record) {
        (None, _, _) => None,
        (Some(_), Source::File(s), _) if record.is_some() || output.is_some() => {
            Some(diarize::Audio::Samples(s.clone(), first_rate))
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
    if let Some(screen) = screen.as_mut() {
        if !screen.wait_loading(&view(&device), || load.is_finished())? {
            // Quit before anything was written; the mic is just dropped.
            return Ok(());
        }
    }
    let Loaded {
        offline,
        zipformer,
        tagger,
    } = load
        .join()
        .map_err(|_| anyhow::anyhow!("model loading panicked"))??;

    let title = cli
        .title
        .clone()
        .unwrap_or_else(|| match cli.input.first() {
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
    let subtitles = match &record {
        Some(r) => vec![
            SubtitleWriter::create(r.srt.clone(), Format::Srt, false)?,
            SubtitleWriter::create(r.ass.clone(), Format::Ass, false)?,
        ],
        None => Vec::new(),
    };
    // The `a` menu only changes the default input: sources named on the
    // command line stay as given.
    let switchable =
        cli.input.is_empty() && cli.device.is_empty() && cli.app.is_empty() && !cli.virtual_sink;
    // Several sources are mixed into the one recording; so is switchable
    // input, which can gain sources mid-session.
    let mixer = match &record {
        Some(r) if per_source || switchable => {
            let rates: Vec<u32> = srcs.iter().map(|(_, rate, _)| *rate).collect();
            Some(Arc::new(Mutex::new(sources::Mixer::create(
                &r.opus, &rates,
            )?)))
        }
        _ => None,
    };

    let manual = cli.manual || cli.once || cli.chat.is_some();
    let mut tagger = tagger;
    let engines: Vec<Engine> = srcs
        .iter()
        .map(|(_, rate, _)| {
            new_engine(
                *rate,
                manual,
                cli.chat.is_some(),
                zipformer.as_ref(),
                &offline,
                &corrector,
                tagger.take(),
            )
        })
        .collect();
    let engine = sources::Engines::new(engines);
    let loaded = started.elapsed();
    let paused = Arc::new(AtomicBool::new(false));
    let muted = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let source_done = Arc::new(AtomicBool::new(false));
    // File sources still being pushed; the last one sets `source_done`.
    let files_left = Arc::new(std::sync::atomic::AtomicUsize::new(srcs.len()));

    // Samples fed per source, with its rate, for the end-of-run stats.
    let mut fed: Vec<(Arc<std::sync::atomic::AtomicU64>, u32)> = Vec::new();
    let mut slots = Vec::new();
    let mut pumps = Vec::new();
    for (i, (source, rate, name)) in srcs.into_iter().enumerate() {
        let recording = match (&mixer, &record) {
            (Some(m), _) => Some(Recorder::Mixed(m.clone(), i)),
            (None, Some(r)) => Some(Recorder::Own(OpusWriter::create(&r.opus, rate)?)),
            (None, None) => None,
        };
        let opts = match &source {
            Source::Live(_, opts) => opts.clone(),
            Source::File(_) => OpenOptions::default(),
        };
        let (source, capture) = match source {
            Source::Live(cap, _) => (None, Some(cap)),
            Source::File(samples) => (Some(samples), None),
        };
        let slot = Arc::new(Slot::new(opts, capture, rate, name));
        fed.push((slot.fed.clone(), rate));
        let mut feed = Feed {
            pusher: engine.pusher(i),
            recording,
            fed: slot.fed.clone(),
        };
        let (paused, stop, done) = (paused.clone(), stop.clone(), source_done.clone());
        let files_left = files_left.clone();
        // The last file to finish marks the input done.
        let file_done = move || {
            if files_left.fetch_sub(1, Ordering::SeqCst) == 1 {
                done.store(true, Ordering::SeqCst);
            }
        };
        let pump = match source {
            None => {
                let live = LivePump {
                    slot: slot.clone(),
                    rate,
                    feed,
                    paused,
                    muted: muted.clone(),
                    stop,
                    lead: 0,
                };
                std::thread::spawn(move || live.run())
            }
            Some(samples) if cli.live => {
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
                std::thread::spawn(move || {
                    player.run();
                    file_done();
                })
            }
            Some(samples) => std::thread::spawn(move || {
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
                file_done();
            }),
        };
        slots.push(slot);
        pumps.push(pump);
    }
    let spawner = switchable.then(|| Spawner {
        offline: offline.clone(),
        zipformer,
        corrector: corrector.clone(),
        mixer: mixer.clone(),
    });
    let labelled = engine.labelled();

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
        paused,
        muted,
        manual: AtomicBool::new(manual),
        llm,
        once: cli.once,
        finish_requested,
        stop,
        source_done,
        slots: Mutex::new(slots),
        spawner,
        history,
        input_ms,
        live: cli.live,
        diarize: cli.diarize,
        recording: record.is_some(),
        num_speakers,
        pumps: Mutex::new(pumps),
    };
    if let (Some(name), Some(screen)) = (&cli.chat, screen.take()) {
        let voice = chat_voice(&models_dir, &cli.voices)?;
        return chat::run(screen, session, &chatdb::path_for(name), voice);
    }
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
            diarized = Some(t0.elapsed());
            match result {
                Ok(t) => {
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
                Err(e) => {
                    say!("diarization failed, files keep the live speaker labels: {e:#}")
                }
            }
        }
    }
    // Speakers renamed during the session: the files were written with
    // the old names as they went, so write them again. Several sources
    // harden out of order, so their files are rewritten in time order.
    let per_source = labelled.load(Ordering::SeqCst);
    if (speakers::renamed() || per_source) && !rewritten && !cli.append {
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
        let audio_ms = fed
            .iter()
            .map(|(n, rate)| n.load(Ordering::Relaxed) * 1000 / (*rate).max(1) as u64)
            .max()
            .unwrap_or(0);
        let offline = !cli.input.is_empty() && !cli.live;
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
    recording: Option<Recorder>,
    /// Samples pushed, for the end-of-run stats.
    fed: Arc<std::sync::atomic::AtomicU64>,
}

impl Feed {
    fn push(&mut self, mono: &[f32]) {
        self.fed.fetch_add(mono.len() as u64, Ordering::Relaxed);
        self.pusher.push(mono);
        match &mut self.recording {
            Some(Recorder::Own(r)) => {
                if let Err(e) = r.write(mono) {
                    tracing::error!("recording stopped: {e:#}");
                    self.recording = None;
                }
            }
            Some(Recorder::Mixed(m, i)) => m.lock().expect("mixer lock").push(*i, mono),
            None => {}
        }
    }

    fn finish(self) {
        match self.recording {
            Some(Recorder::Own(r)) => {
                if let Err(e) = r.finish() {
                    tracing::error!("finishing the recording failed: {e:#}");
                }
            }
            Some(Recorder::Mixed(m, i)) => m.lock().expect("mixer lock").done(i),
            None => {}
        }
    }
}

/// The --record file: one source's own, or a share of the mix.
enum Recorder {
    Own(OpusWriter),
    Mixed(Arc<Mutex<sources::Mixer>>, usize),
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
    slot: Arc<Slot>,
    /// The engine's rate; an input reopened at another rate is resampled.
    rate: u32,
    feed: Feed,
    paused: Arc<AtomicBool>,
    muted: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    /// Silence to feed the engine first: a source added mid-session
    /// starts at the session's current time.
    lead: u64,
}

/// How long an app capture may give pure digital silence before we check
/// whether the app is playing (and so the tap is likely denied).
const HUSH: Duration = Duration::from_secs(4);

/// Watches an app capture for the silence macOS returns when the
/// terminal lacks System Audio Recording permission.
#[derive(Default)]
struct Hush {
    since: Option<std::time::Instant>,
    /// Real sound arrived, or the warning was given: stop watching.
    done: bool,
}

type Chunks = crossbeam_channel::Receiver<Vec<f32>>;

impl LivePump {
    fn run(mut self) {
        let second = vec![0.0; self.rate as usize];
        let mut lead = self.lead;
        while lead > 0 && !self.stop.load(Ordering::Relaxed) {
            let n = lead.min(second.len() as u64) as usize;
            self.feed.pusher.push(&second[..n]);
            lead -= n as u64;
        }
        let mut chunks = self.chunks();
        let mut resampler: Option<vox_audio::resample::Linear> = None;
        // Silence pushed since the input was released: (since, samples).
        let mut silent: Option<(std::time::Instant, u64)> = None;
        // The input was switched in the `a` menu (vs. resuming a pause).
        let mut switched = false;
        let mut hush = Hush::default();
        while !self.stop.load(Ordering::Relaxed) {
            if self.slot.reopen.swap(false, Ordering::SeqCst) {
                chunks = None;
                self.slot.capture.lock().expect("capture lock").take();
                silent.get_or_insert((std::time::Instant::now(), 0));
                switched = true;
                hush = Hush::default();
            }
            let off = self.slot.off.load(Ordering::Relaxed);
            if self.paused.load(Ordering::Relaxed) || off {
                // A virtual sink stays up while paused so apps playing
                // into it aren't rerouted; its audio is replaced with
                // silence.
                let keep = !off && self.slot.opts.lock().expect("opts lock").virtual_sink;
                if chunks.is_some() && !keep {
                    chunks = None;
                    self.slot.capture.lock().expect("capture lock").take();
                    info!(off, "input released");
                }
                if let Some(rx) = &chunks {
                    if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(100)) {
                        self.push(&vec![0.0; chunk.len()], &mut resampler);
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
                    Ok((rx, rs)) => {
                        chunks = Some(rx);
                        resampler = rs;
                    }
                    Err(e) if switched => {
                        tracing::warn!("switching input failed: {e:#}");
                        *self.slot.notice.lock().expect("notice lock") =
                            Some(format!("couldn't switch input: {e:#}"));
                        self.slot.off.store(true, Ordering::SeqCst);
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!("reopening the microphone failed: {e:#}");
                        self.paused.store(true, Ordering::SeqCst);
                        continue;
                    }
                }
                switched = false;
                // Cover the time spent reopening.
                if let Some((since, mut pushed)) = silent.take() {
                    self.catch_up(since, &mut pushed);
                }
            }
            let Some(rx) = &chunks else { continue };
            if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(100)) {
                if self.muted.load(Ordering::Relaxed) {
                    self.push(&vec![0.0; chunk.len()], &mut resampler);
                    continue;
                }
                self.watch(&mut hush, &chunk);
                self.push(&chunk, &mut resampler);
            }
        }
        self.feed.finish();
    }

    /// An app capture that stays exactly zero while the app is playing:
    /// say the terminal likely needs System Audio Recording permission.
    fn watch(&self, hush: &mut Hush, chunk: &[f32]) {
        if hush.done {
            return;
        }
        let Some(want) = self.slot.opts.lock().expect("opts lock").app.clone() else {
            hush.done = true;
            return;
        };
        if chunk.iter().any(|&x| x != 0.0) {
            hush.done = true;
            return;
        }
        let since = *hush.since.get_or_insert_with(std::time::Instant::now);
        if since.elapsed() < HUSH {
            return;
        }
        hush.since = None;
        let playing = vox_audio::default_backend()
            .and_then(|b| b.list_apps())
            .map(|apps| apps.iter().any(|a| a.playing && a.matches(&want)))
            .unwrap_or(false);
        if playing {
            hush.done = true;
            let msg = format!(
                "{want:?} is silent: allow this terminal System Audio Recording (Privacy & Security)"
            );
            tracing::warn!("{msg}");
            *self.slot.notice.lock().expect("notice lock") = Some(msg);
        }
    }

    fn push(&mut self, chunk: &[f32], resampler: &mut Option<vox_audio::resample::Linear>) {
        match resampler {
            Some(r) => {
                let mut out = Vec::with_capacity(chunk.len() * 2);
                r.process(chunk, &mut out);
                self.feed.push(&out);
            }
            None => self.feed.push(chunk),
        }
    }

    fn chunks(&self) -> Option<Chunks> {
        let cap = self.slot.capture.lock().expect("capture lock");
        cap.as_ref().map(|c| c.chunks().clone())
    }

    /// Open the slot's input, with a resampler if it runs at another
    /// rate than the engine.
    fn reopen(&self) -> Result<(Chunks, Option<vox_audio::resample::Linear>)> {
        let opts = self.slot.opts.lock().expect("opts lock").clone();
        let cap = vox_audio::default_backend()?.open(&opts)?;
        let resampler = (cap.sample_rate != self.rate)
            .then(|| vox_audio::resample::Linear::new(cap.sample_rate, self.rate));
        let rx = cap.chunks().clone();
        *self.slot.name.lock().expect("name lock") = cap.device.clone();
        info!(device = %cap.device, rate = cap.sample_rate, "input opened");
        *self.slot.capture.lock().expect("capture lock") = Some(cap);
        Ok((rx, resampler))
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
            session.device(),
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

/// The voice --chat replies in: the first of `--voices`, else af_heart.
fn chat_voice(models_dir: &Path, voices: &[String]) -> Result<voice::Voice> {
    let name = voices.first().map(String::as_str).unwrap_or("af_heart");
    let sid =
        vox_transcribe::tts::voice_id(name).with_context(|| format!("no Kokoro voice {name:?}"))?;
    let threads = std::thread::available_parallelism()
        .map(|n| n.get().min(4))
        .unwrap_or(2) as i32;
    let tts = vox_transcribe::tts::Kokoro::load(&models::kokoro_dir(models_dir), threads)?;
    voice::Voice::new(tts, sid)
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
    let quiet = !cli.input.is_empty() && !cli.live && cli.log.is_none();
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
