//! `vox_room`: a voice room in the browser. People log in with an invite
//! link, talk through their headsets, hear each other and an AI that
//! talks back, and share one live transcript. See ROOM.md.

mod agent;
mod interp;
mod mixer;
mod room;
mod rooms;
mod screen;
mod store;
mod stt;
mod tts;
mod web;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use clap::{CommandFactory as _, Parser, Subcommand, ValueEnum};
use tracing::{info, warn};

use crate::room::{Room, RoomConfig};
use crate::store::Store;

#[derive(Parser)]
#[command(name = "vox_room", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,

    /// Address to serve on. Put it behind a reverse proxy with HTTPS for
    /// anyone not on this machine: browsers only allow the mic on HTTPS
    /// (or localhost).
    #[arg(
        long,
        env = "VOX_ROOM_LISTEN",
        default_value = "127.0.0.1:7340",
        global = true
    )]
    listen: SocketAddr,
    /// The room's public address, for invite links. Default: the listen
    /// address.
    #[arg(long, env = "VOX_ROOM_URL", global = true)]
    url: Option<String>,
    /// Where the room keeps its database.
    #[arg(long, env = "VOX_ROOM_DATA", global = true)]
    data_dir: Option<PathBuf>,

    /// Speech-to-text models (the same ones scribe downloads).
    #[arg(long, env = "VOX_SCRIBE_MODELS")]
    models_dir: Option<PathBuf>,
    /// Run without transcription (and so without the AI hearing anyone).
    #[arg(long)]
    no_stt: bool,

    /// The rooms a *new* house starts with, the first of them the
    /// lobby. Only read when the database has no rooms yet: after the
    /// first start the house is the database's, and the owner makes
    /// and unmakes rooms from the room itself (see ROOM.md). Everyone
    /// also has a room of their own, always, which is not named here
    /// -- it is made the first time they go into it.
    #[arg(
        long = "room",
        env = "VOX_ROOM_ROOMS",
        value_delimiter = ',',
        default_value = "Lobby"
    )]
    rooms: Vec<String>,

    /// Another AI in every room, as `NAME`, `NAME:VOICE` or
    /// `NAME:VOICE:PROMPT_FILE`. Repeat it for more.
    ///
    /// The first AI is the one `--name`, `--voice`, `--alias` and
    /// `--prompt` describe; these are the ones beside it. Each has its
    /// own voice, its own fader, and its own say in when it speaks —
    /// and at a pause every one of them that could answer is given the
    /// turn at once, with the first to actually say something keeping
    /// it and the rest told to stop. Two or three is the sane number:
    /// each one judging each pause is a request each.
    #[arg(long = "bot", env = "VOX_ROOM_BOTS", value_delimiter = ',')]
    bots: Vec<String>,

    /// The AI's name. In "named" mode it answers when someone says it.
    #[arg(long, env = "VOX_ROOM_AGENT_NAME", default_value = "Vox")]
    name: String,
    /// Other spellings that count as its name, for what speech-to-text
    /// makes of it (repeat, or comma-separate in the variable).
    #[arg(long = "alias", env = "VOX_ROOM_AGENT_ALIASES", value_delimiter = ',')]
    aliases: Vec<String>,
    /// Quiet after the last words before what was said is handed over and
    /// the AI may answer.
    #[arg(long, env = "VOX_ROOM_REPLY_AFTER_MS", default_value_t = 1200)]
    reply_after_ms: u64,
    /// Continuous speech that cuts the AI off when someone talks over it
    /// (a first recognised word cuts it sooner). Until then it is only
    /// turned down.
    #[arg(long, env = "VOX_ROOM_CONFIRM_MS", default_value_t = 300)]
    confirm_ms: u64,
    /// Idle time that ends a back-and-forth: once someone has said the
    /// AI's name in "when named" mode it answers every pause, until the
    /// room has been this quiet.
    #[arg(long, env = "VOX_ROOM_ENGAGED_FOR_MS", default_value_t = 30_000)]
    engaged_for_ms: u64,
    /// Entries of the conversation sent to the model with each turn.
    #[arg(long, env = "VOX_ROOM_HISTORY", default_value_t = 60)]
    history: usize,

    /// OpenAI-compatible base URL ending in /v1. The same service as
    /// scribe's --llm and `agent`; the key comes from VOX_SCRIBE_LLM_KEY.
    #[arg(long, env = "VOX_SCRIBE_LLM_URL", default_value = vox_transcribe::openai::DEFAULT_URL)]
    llm_url: String,
    /// Model name.
    #[arg(long, env = "VOX_SCRIBE_LLM_MODEL")]
    model: Option<String>,
    /// How hard the model may think before it answers: none, low,
    /// medium or high. Never spoken, and it costs latency either way.
    /// On a local server this turns its chat template's thinking on or
    /// off; on OpenAI it is the reasoning effort.
    #[arg(long, env = "VOX_ROOM_REASONING", default_value = "low")]
    reasoning: String,
    /// Think before answering: the old name for `--reasoning medium`.
    /// VOX_AGENT_THINK is still read, leniently (see `thinks`).
    #[arg(long)]
    think: bool,
    /// Which API to speak: auto, responses or chat. `/responses` is the
    /// one that takes tools and thinking together, which OpenAI's
    /// reasoning models refuse on `/chat/completions`; a local llama.cpp
    /// or vLLM has only `/chat/completions`. `auto` tries `/responses`
    /// and remembers if there isn't one.
    #[arg(long, env = "VOX_ROOM_LLM_API", default_value = "auto")]
    llm_api: String,
    /// Let the AI look things up in the lobby, by asking the endpoint's
    /// own web search (OpenAI's `/responses`). Off by default: a local
    /// llama.cpp has no such thing, and each search costs a request.
    #[arg(long, env = "VOX_ROOM_SEARCH")]
    search: bool,
    /// No model: the AI says back what it heard. For trying the room.
    #[arg(long)]
    echo: bool,
    /// System prompt; `{name}` and `{people}` are filled in.
    #[arg(long, env = "VOX_ROOM_PROMPT", conflicts_with = "prompt_file")]
    prompt: Option<String>,
    #[arg(long, env = "VOX_ROOM_PROMPT_FILE")]
    prompt_file: Option<PathBuf>,

    /// The AI's voice. Default: qwen3 when --tts-url is set, else kokoro
    /// when its model is downloaded, else none.
    #[arg(long, env = "VOX_ROOM_TTS")]
    tts: Option<TtsKind>,
    /// A Qwen3-TTS server (vllm-omni's /v1/audio/speech), as for pi-tts.
    #[arg(long, env = "VOX_ROOM_TTS_URL")]
    tts_url: Option<String>,
    /// Bearer token for the Qwen3-TTS server, if it wants one.
    #[arg(long, env = "VOX_ROOM_TTS_KEY")]
    tts_key: Option<String>,
    /// Model to ask that server for. Leave it out and the request doesn't
    /// name one, which is right for a server with a single model.
    #[arg(long, env = "VOX_ROOM_TTS_MODEL")]
    tts_model: Option<String>,
    /// Qwen3 voice.
    #[arg(long, env = "VOX_ROOM_VOICE", default_value = "aiden")]
    voice: String,
    /// How the Qwen3 voice should speak, e.g. "calm and warm".
    #[arg(long, env = "VOX_ROOM_INSTRUCT", default_value = "")]
    instruct: String,
    #[arg(long, env = "VOX_ROOM_LANGUAGE", default_value = "English")]
    language: String,
    /// Other languages the door offers, comma-separated and spelled out
    /// in English as Qwen3-TTS wants them ("Russian"). With any of them
    /// chosen, the AI answers in each language the room is listening in,
    /// and the conversation is kept in --language for the model. Needs
    /// `--tts qwen3`: it is the only voice here that speaks more than
    /// one language.
    #[arg(
        long = "language-also",
        env = "VOX_ROOM_LANGUAGES",
        value_delimiter = ','
    )]
    languages: Vec<String>,
    /// Say what people say to the people who don't speak it: a voice in
    /// each language the room is listening in, reading an utterance a
    /// phrase behind the person who said it, with the voices it is
    /// interpreting ducked under it. Needs `--tts qwen3` and a
    /// translator, like the AI's own languages do.
    #[arg(
        long,
        env = "VOX_ROOM_INTERPRET",
        default_value_t = true,
        action = clap::ArgAction::Set
    )]
    interpret: bool,
    /// The interpreters' Qwen3 voice. Different from `--voice` on
    /// purpose: an interpretation is not the AI talking, and the one
    /// thing a listener needs to be able to tell is which is which.
    #[arg(long, env = "VOX_ROOM_INTERPRETER_VOICE", default_value = "ryan")]
    interpreter_voice: String,
    /// Kokoro voice.
    #[arg(long, env = "VOX_ROOM_KOKORO_VOICE", default_value = "am_michael")]
    kokoro_voice: String,
    /// Who translates, when the room is listening in more than one
    /// language. `openai` asks the same model that writes the replies:
    /// it follows instructions, so a URL or a command comes back
    /// untouched, and it can be shown the lines before this one -- but
    /// it costs a round trip, about two seconds a sentence. `local`
    /// asks Marian models on the CPU, about forty times faster and with
    /// no network at all, but it only leaves alone what is put in
    /// backticks. Needs the models: see ROOM.md.
    #[arg(long, env = "VOX_ROOM_TRANSLATE", default_value = "openai")]
    translate: TranslateKind,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print the one-time invite link for NAME, making one if needed.
    Invite { name: String },
    /// Make NAME the owner and print their link. The owner invites
    /// everybody else and says which rooms they may go to, from inside
    /// the room. There is one; `uninvite` the old one first.
    InviteOwner { name: String },
    /// Revoke NAME: their link and their browsers' cookies.
    Uninvite { name: String },
    /// List everyone invited, with the links not yet used.
    People,
}

/// Who puts a line into another language.
#[derive(Clone, Copy, ValueEnum, PartialEq)]
enum TranslateKind {
    /// The same chat model that writes the replies.
    Openai,
    /// Marian models on this machine, through CTranslate2.
    Local,
}

#[derive(Clone, Copy, ValueEnum, PartialEq)]
enum TtsKind {
    Qwen3,
    Kokoro,
    None,
}

/// The languages Qwen3-TTS answers to, spelled the way it wants them.
/// Everything downstream matches on these strings -- the voice asked for
/// a language, the pass-1 model looked up by one -- so they are settled
/// here, once, rather than compared loosely in each place.
const LANGUAGES: [&str; 11] = [
    "Auto",
    "Chinese",
    "English",
    "German",
    "Italian",
    "Portuguese",
    "Spanish",
    "Japanese",
    "Korean",
    "French",
    "Russian",
];

/// `name` as [`LANGUAGES`] spells it, whatever case it was given in.
/// An unknown language is left alone and warned about: the list is what
/// this Qwen3 build is known to speak, not a rule about what exists.
fn canonical_language(name: &str) -> String {
    let name = name.trim();
    match LANGUAGES.iter().find(|l| l.eq_ignore_ascii_case(name)) {
        Some(l) => l.to_string(),
        None => {
            warn!(
                language = name,
                "not a language this voice is known to speak; \
                 one of: {}",
                LANGUAGES.join(", ")
            );
            name.to_string()
        }
    }
}

/// How to make a voice, for every room on the server. The settings are
/// the same wherever it is speaking, so a call only says what language
/// and which preset.
struct Speakers {
    kind: TtsKind,
    url: Option<String>,
    key: Option<String>,
    model: Option<String>,
    instruct: String,
    /// The one Kokoro, when that is the voice: it is the whole model,
    /// so every room reads through this copy. None for the others,
    /// which are cheap enough to make per room.
    kokoro: Option<tts::Shared>,
}

impl Speakers {
    /// Whether a voice of this kind can be asked for a language other
    /// than its own, which is what says whether the other languages the
    /// door offers are ever really spoken.
    fn multilingual(&self) -> bool {
        self.kind == TtsKind::Qwen3
    }

    fn voice(&self, language: &str, preset: &str) -> Result<Box<dyn tts::Voice>> {
        Ok(match self.kind {
            TtsKind::Qwen3 => {
                let Some(url) = self.url.clone() else {
                    bail!("--tts qwen3 needs --tts-url (or VOX_ROOM_TTS_URL)");
                };
                Box::new(tts::Qwen3::new(
                    &url,
                    preset,
                    &self.instruct,
                    language,
                    self.key.clone(),
                    self.model.clone(),
                ))
            }
            TtsKind::Kokoro => match &self.kokoro {
                Some(shared) => shared.handle(),
                None => Box::new(tts::Silent),
            },
            TtsKind::None => Box::new(tts::Silent),
        })
    }
}

/// The local translator, when this build has one. Behind a feature
/// because it brings a C++ library and a CMake build with it; the flag
/// says plainly what to do when it isn't there, rather than quietly
/// falling back to the network the operator asked to do without.
#[cfg(feature = "translate-local")]
fn local_translator(
    models_dir: &std::path::Path,
    languages: &[String],
) -> Result<Arc<dyn vox_transcribe::translate::Translate>> {
    let t = vox_transcribe::ct2::Ct2Translator::load(models_dir, languages)?;
    info!(directions = %t.directions().join(", "), "translating: locally, on the CPU");
    Ok(Arc::new(t))
}

#[cfg(not(feature = "translate-local"))]
fn local_translator(
    _models_dir: &std::path::Path,
    _languages: &[String],
) -> Result<Arc<dyn vox_transcribe::translate::Translate>> {
    bail!("--translate local needs a build with the `translate-local` feature")
}

fn data_dir(cli: &Cli) -> PathBuf {
    if let Some(d) = &cli.data_dir {
        return d.clone();
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/vox_room")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("vox_room")
    }
}

/// Every environment variable the command line reads, including the
/// subcommands' (they have none today, but this way it can't rot).
fn env_names(cmd: &clap::Command, out: &mut Vec<OsString>) {
    out.extend(
        cmd.get_arguments()
            .filter_map(|a| a.get_env())
            .map(OsString::from),
    );
    for sub in cmd.get_subcommands() {
        env_names(sub, out);
    }
}

/// Forget the variables that are set but blank, before clap reads them.
///
/// `FOO=` in a `.env` file or a compose service is how you say "leave
/// this one alone", but to clap a set-but-empty variable is a value like
/// The old `VOX_AGENT_THINK`, read by hand and leniently: as a clap flag
/// anything but exactly `true` or `false` stopped the room from starting
/// at all, which an old `.env` saying `0` should not do.
fn thinks(flag: bool) -> bool {
    flag || std::env::var("VOX_AGENT_THINK").is_ok_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// any other: a blank `VOX_ROOM_PROMPT` becomes an empty system prompt, a
/// blank `VOX_ROOM_TTS_URL` a voice server at no address, and a blank
/// `VOX_ROOM_TTS` won't parse at all, so the room refuses to start.
fn forget_blank_env() {
    let mut names = Vec::new();
    env_names(&Cli::command(), &mut names);
    for name in names {
        if std::env::var(&name).is_ok_and(|v| v.trim().is_empty()) {
            std::env::remove_var(&name);
        }
    }
}

fn base_url(cli: &Cli) -> String {
    cli.url
        .clone()
        .unwrap_or_else(|| format!("http://{}", cli.listen))
        .trim_end_matches('/')
        .to_string()
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,ort=warn".into()),
        )
        .init();
    forget_blank_env();
    let cli = Cli::parse();
    let db = data_dir(&cli).join("room.db");
    let store = Store::open(&db)?;
    match &cli.command {
        Some(Cmd::Invite { name }) => {
            let name = check_person_name(name)?;
            // A second link for somebody who has already traded one in
            // is a deliberate thing (another browser), so say so.
            if store.person(name)?.is_some_and(|p| p.link.is_none()) {
                eprintln!("{name} has joined already; this is a second link for them.");
            }
            println!("{}/join/{}", base_url(&cli), store.invite(name)?);
            return Ok(());
        }
        Some(Cmd::InviteOwner { name }) => {
            let name = check_person_name(name)?;
            // invite_owner refuses while somebody else is the owner,
            // and says who to uninvite.
            let token = store.invite_owner(name)?;
            println!("{}/join/{token}", base_url(&cli));
            eprintln!(
                "{name} is the owner: they invite everybody else and say which rooms \
                 people may go to, from the room itself."
            );
            return Ok(());
        }
        Some(Cmd::Uninvite { name }) => {
            let was_owner = store.owner()?.as_deref() == Some(name.as_str());
            if !store.uninvite(name)? {
                bail!("{name} wasn't invited");
            }
            println!(
                "{name}'s link and cookie are gone (an open tab stays connected until it reloads)"
            );
            if was_owner {
                eprintln!(
                    "The room has no owner now: `vox_room invite-owner NAME` to give it one."
                );
            }
            return Ok(());
        }
        Some(Cmd::People) => {
            if store.owner()?.is_none() {
                eprintln!("Nobody owns this room: `vox_room invite-owner NAME` to give it one.");
            }
            for p in store.people()? {
                let mut who = if p.display == p.name {
                    p.name.clone()
                } else {
                    format!("{} (in the room as {})", p.name, p.display)
                };
                if p.owner {
                    who.push_str(" [owner]");
                }
                match p.link {
                    Some(token) => println!("{who}\t{}/join/{token}", base_url(&cli)),
                    None => println!("{who}\tjoined; `vox_room invite {}` for a new link", p.name),
                }
            }
            return Ok(());
        }
        None => {}
    }
    serve(cli, store, db)
}

/// A name somebody can be invited under: `vox_room uninvite` names it,
/// and `@` opens a room of one's own.
fn check_person_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() || name.starts_with('@') {
        bail!("give the person a name");
    }
    if name.chars().count() > room::NAME_MAX {
        bail!("names are at most {} characters", room::NAME_MAX);
    }
    Ok(name)
}

fn serve(cli: Cli, store: Store, db: PathBuf) -> Result<()> {
    if store.people()?.is_empty() {
        warn!("nobody is invited yet: run `vox_room invite NAME` for a link");
    }
    match store.owner()? {
        Some(owner) => info!(%owner, "the owner invites people and hands out the rooms"),
        // Everything still works; what is missing is the managing of it
        // from a browser, and nothing else can be done about that from
        // one -- which is the point of its being a command.
        None => warn!(
            "nobody owns this room: run `vox_room invite-owner NAME` to let somebody \
             invite people and hand out rooms from the room itself"
        ),
    }
    // A database written before there was more than one room: what was
    // the whole server's -- the conversation, the mode, the faders --
    // is the first room's. Done before any room opens, so none of them
    // reads a half-moved setting.
    let first = cli.rooms.first().map(|r| r.trim()).unwrap_or_default();
    if first.is_empty() {
        bail!("--room NAME, at least once");
    }
    store.adopt(first)?;
    drop(store);
    let llm = if cli.echo {
        agent::Llm::Echo
    } else {
        let Some(model) = cli.model.clone() else {
            bail!("set VOX_SCRIBE_LLM_MODEL (or --model), or pass --echo to try the room without a model");
        };
        let endpoint = vox_transcribe::openai::Endpoint::at(cli.llm_url.clone());
        if cli.search {
            info!("the AI can look things up in the lobby, with the endpoint's web search");
        }
        // --think was the old name, and meant "think about it".
        let reasoning = match agent::Reasoning::parse(&cli.reasoning) {
            Some(_) if thinks(cli.think) && cli.reasoning == "low" => agent::Reasoning::Medium,
            Some(r) => r,
            None => bail!(
                "--reasoning is none, low, medium or high, not {:?}",
                cli.reasoning
            ),
        };
        let Some(api) = agent::Api::parse(&cli.llm_api) else {
            bail!(
                "--llm-api is auto, responses or chat, not {:?}",
                cli.llm_api
            );
        };
        info!(
            reasoning = reasoning.as_str(),
            api = ?api,
            "the AI's endpoint"
        );
        agent::Llm::Chat(agent::Chat {
            url: endpoint.url,
            model,
            key: endpoint.api_key,
            reasoning,
            api,
            search: cli.search,
        })
    };
    let prompt = match (&cli.prompt, &cli.prompt_file) {
        (Some(p), _) => p.clone(),
        (None, Some(f)) => {
            std::fs::read_to_string(f).with_context(|| format!("read {}", f.display()))?
        }
        (None, None) => agent::DEFAULT_PROMPT.to_string(),
    };

    let models_dir = cli
        .models_dir
        .clone()
        .unwrap_or_else(stt::default_models_dir);
    let kokoro_dir = models_dir.join("kokoro");
    let tts_url = cli
        .tts_url
        .clone()
        .or_else(|| std::env::var("PI_TTS_URL").ok().filter(|u| !u.is_empty()));
    let kind = cli.tts.unwrap_or(if tts_url.is_some() {
        TtsKind::Qwen3
    } else if vox_transcribe::tts::has_kokoro(&kokoro_dir) {
        TtsKind::Kokoro
    } else {
        TtsKind::None
    });
    // The room's own language first, then the others the door offers,
    // each at most once however it was spelled -- and settled to the one
    // spelling everything downstream matches on, so `english` in the
    // environment can't quietly miss the model filed under `English`.
    let language = canonical_language(&cli.language);
    let cfg_languages: Vec<String> = std::iter::once(language.clone())
        .chain(
            cli.languages
                .iter()
                .filter(|l| !l.trim().is_empty())
                .map(|l| canonical_language(l)),
        )
        .filter(|l| !l.is_empty())
        .fold(Vec::new(), |mut out, l| {
            if !out.iter().any(|k: &String| k.eq_ignore_ascii_case(&l)) {
                out.push(l);
            }
            out
        });

    // What the voices can actually say, which is not always what the
    // door offers: one voice means one language, however many are on
    // the list. Settled here, once, because it goes into the config
    // every room is built from.
    let speakers = Speakers {
        kind,
        url: tts_url.clone(),
        key: cli.tts_key.clone(),
        model: cli.tts_model.clone(),
        instruct: cli.instruct.clone(),
        // Loaded now and shared by every room: see `tts::Shared`.
        kokoro: match kind {
            TtsKind::Kokoro => {
                info!(voice = %cli.kokoro_voice, "voice: Kokoro");
                Some(tts::Shared::new(Box::new(tts::Kokoro::load(
                    &kokoro_dir,
                    &cli.kokoro_voice,
                )?)))
            }
            _ => None,
        },
    };
    let spoken: Vec<String> = if speakers.multilingual() {
        cfg_languages.clone()
    } else {
        let others: Vec<&str> = cfg_languages
            .iter()
            .filter(|l| **l != language)
            .map(String::as_str)
            .collect();
        if !others.is_empty() {
            // Said once, loudly: the door would otherwise offer languages
            // that quietly come out in the room's own.
            warn!(
                languages = %others.join(", "),
                "this voice speaks only {language}: the other languages are \
                 written down, and said in {language} (use --tts qwen3 to have \
                 them spoken)",
            );
        }
        vec![language.clone()]
    };
    match kind {
        TtsKind::Qwen3 if tts_url.is_none() => {
            bail!("--tts qwen3 needs --tts-url (or VOX_ROOM_TTS_URL)")
        }
        TtsKind::Qwen3 => info!(
            url = tts_url.as_deref().unwrap_or_default(),
            voice = %cli.voice,
            model = cli.tts_model.as_deref().unwrap_or("(unnamed)"),
            key = cli.tts_key.is_some(),
            languages = %spoken.join(", "),
            "voice: Qwen3-TTS"
        ),
        TtsKind::Kokoro => {}
        TtsKind::None => {
            warn!("no voice: the AI's replies are shown, silently, at reading pace")
        }
    }

    let factory = if cli.no_stt {
        // Said out loud, because from inside the room it looks like a
        // fault rather than a choice: everyone hears everyone, the
        // meters move, and not a word is written down.
        warn!(
            "--no-stt: nothing anyone says is transcribed, so there is no \
             conversation and the AI cannot hear the room"
        );
        None
    } else {
        Some(stt::Models::load(&models_dir)?.factory())
    };

    // Who translates, when there is more than one language and a model
    // to do it with. Built once and shared by every room: the local one
    // is 75 MB of weights per direction, and every voice thread on the
    // server reads the same copy.
    let translate: Option<Arc<dyn vox_transcribe::translate::Translate>> = match cli.translate {
        _ if cfg_languages.len() < 2 => None,
        TranslateKind::Openai => match &llm {
            agent::Llm::Chat(c) => {
                info!(model = %c.model, "translating: the reply model");
                Some(Arc::new(vox_transcribe::openai::OpenAiTranslator::new(
                    vox_transcribe::openai::OpenAiConfig {
                        base_url: c.url.clone(),
                        model: c.model.clone(),
                        api_key: c.key.clone(),
                        vocabulary: Vec::new(),
                        // Shorter than a reply's: a translation sits in
                        // front of something somebody is waiting to hear.
                        timeout: Duration::from_secs(30),
                    },
                )))
            }
            // --echo has no model to ask.
            agent::Llm::Echo => None,
        },
        TranslateKind::Local => Some(local_translator(&models_dir, &cfg_languages)?),
    };
    if cli.interpret && translate.is_some() && spoken.len() > 1 {
        info!(
            languages = %spoken.join(", "),
            voice = %cli.interpreter_voice,
            "interpreting: what people say is spoken to whoever doesn't speak it"
        );
    }

    // The AIs every room gets. The first is the one the old flags
    // describe; `--bot` adds the others, each with a voice and a
    // prompt of its own.
    let mut bots = vec![room::BotConfig::new(
        &cli.name,
        cli.aliases
            .iter()
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty())
            .collect(),
    )];
    let mut voices: BTreeMap<String, String> =
        BTreeMap::from([(cli.name.clone(), cli.voice.clone())]);
    let mut prompts: BTreeMap<String, String> = BTreeMap::new();
    for spec in cli.bots.iter().filter(|b| !b.trim().is_empty()) {
        let mut parts = spec.splitn(3, ':');
        let name = parts.next().unwrap_or_default().trim().to_string();
        if name.is_empty() {
            bail!("--bot wants a name, as NAME or NAME:VOICE or NAME:VOICE:PROMPT_FILE");
        }
        if bots.iter().any(|b| b.name.eq_ignore_ascii_case(&name)) {
            bail!("two AIs called {name:?}: they answer to their names, so they need their own");
        }
        let said = parts.next().unwrap_or("").trim();
        voices.insert(
            name.clone(),
            if said.is_empty() {
                cli.voice.clone()
            } else {
                said.to_string()
            },
        );
        if let Some(file) = parts.next().map(str::trim).filter(|f| !f.is_empty()) {
            let text = std::fs::read_to_string(file)
                .with_context(|| format!("read {name}'s prompt from {file}"))?;
            prompts.insert(name.clone(), text);
        }
        // A bot nobody configured aliases for answers to its own name.
        bots.push(room::BotConfig::new(&name, Vec::new()));
    }
    if bots.len() > 1 {
        info!(
            bots = %bots.iter().map(|b| b.name.as_str()).collect::<Vec<_>>().join(", "),
            "more than one AI in every room: at a pause they are all given the \
             turn, and the first to say something keeps it"
        );
    }

    // The settings every room shares. What makes one room different
    // from another -- its name, who it admits, how its AI takes the
    // turn to begin with -- is filled in by `rooms::Rooms`.
    let base = RoomConfig {
        id: String::new(),
        title: String::new(),
        only: None,
        default_mode: room::Mode::Named,
        bots,
        reply_after: Duration::from_millis(cli.reply_after_ms),
        confirm: Duration::from_millis(cli.confirm_ms),
        engaged_for: Duration::from_millis(cli.engaged_for_ms),
        history: cli.history,
        language: language.clone(),
        languages: cfg_languages.clone(),
        spoken,
    };

    // Everything a room is made of, built on demand: the shared rooms
    // now, somebody's own the first time they walk into it. What it
    // captures is the expensive part, made once -- the loaded models,
    // the translator, the endpoint -- and what it builds per room is
    // the cheap part: a database handle, a mixer, a few threads.
    let make: rooms::Make = {
        let db = db.clone();
        let speakers = Arc::new(speakers);
        let translate = translate.clone();
        let voice = cli.voice.clone();
        let interpreter_voice = cli.interpreter_voice.clone();
        let (voices, prompts) = (voices, prompts);
        let interpret = cli.interpret;
        Box::new(move |cfg: RoomConfig| -> Result<Arc<Room>> {
            // `Room::new` scopes it to the room it is given.
            let store = Store::open(&db)?;
            let languages = cfg.spoken.clone();
            // The two ends of the typing queue are handed out
            // separately: the room is built with the one it puts work
            // on, the translators get the other once there is a room
            // for them to report back to.
            let (typing_tx, typing_rx) = match agent::typing_queue(translate.as_ref()) {
                Some((tx, rx)) => (Some(tx), Some(rx)),
                None => (None, None),
            };
            // The interpreters: one per language the room is listening
            // in, including its own -- a Russian speaker's words have
            // to reach the English listeners as much as the other way
            // about. Same two conditions as the AI's own languages, for
            // the same reasons: a translator to put it into words, and
            // a voice that can say them.
            let mut interp_tx = BTreeMap::new();
            let mut interp_rx = Vec::new();
            // Never in a room of somebody's own: there is one person
            // in it and the AI already answers in their language, so
            // there is nobody for an interpreter to interpret to.
            if interpret && translate.is_some() && languages.len() > 1 && cfg.only.is_none() {
                for language in &languages {
                    let (tx, rx) = interp::queue();
                    interp_tx.insert(language.clone(), tx);
                    interp_rx.push((language.clone(), rx));
                }
            }
            let (room, wiring) = Room::new(store, cfg, typing_tx, interp_tx, factory.clone())?;
            if let Some(rx) = typing_rx {
                if let Some(t) = translate.clone() {
                    agent::spawn_typists(room.clone(), t, rx, 2);
                }
            }
            for (language, rx) in interp_rx {
                let Some(t) = translate.clone() else { break };
                interp::spawn(
                    room.clone(),
                    &language,
                    speakers.voice(&language, &interpreter_voice)?,
                    t,
                    rx,
                );
            }
            // One agent thread per bot, and one voice per language it
            // is spoken in -- each on its own thread, so the languages
            // are said at the same time rather than one after another,
            // and so are the bots.
            for bot in wiring {
                let preset = voices.get(&bot.name).unwrap_or(&voice);
                let mut speaking: Vec<(String, Box<dyn tts::Voice>)> = Vec::new();
                for language in &languages {
                    speaking.push((language.clone(), speakers.voice(language, preset)?));
                }
                agent::Agent {
                    llm: llm.clone(),
                    name: bot.name.clone(),
                    // Its own instructions, or the room's when it was
                    // given none of its own. `{name}` is filled in
                    // either way, so the default already says which of
                    // them it is talking as.
                    prompt: prompts.get(&bot.name).unwrap_or(&prompt).clone(),
                    translate: translate.clone(),
                    generation: bot.generation,
                }
                .spawn(room.clone(), speaking, bot.turns);
            }
            Ok(room)
        })
    };

    // The rooms' own handle on the invite list: who may go where is
    // asked on the way into a room, and the answer is not any one
    // room's to give.
    let rooms = rooms::Rooms::new(Store::open(&db)?, &base, &cli.rooms, make)?;
    rooms.spawn_clocks()?;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(cli.listen)
            .await
            .with_context(|| format!("listen on {}", cli.listen))?;
        info!(
            "room open on http://{} (invite people with `vox_room invite NAME`)",
            cli.listen
        );
        axum::serve(listener, web::router(rooms, screen::Screen::new()))
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
        anyhow::Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test, because it mutates the process environment.
    #[test]
    fn blank_environment_variables_count_as_unset() {
        // A blank one of each shape that used to go wrong: no default, a
        // default to fall back to, and a value that has to parse.
        std::env::set_var("VOX_ROOM_PROMPT", "");
        std::env::set_var("VOX_ROOM_TTS_URL", "   ");
        std::env::set_var("VOX_ROOM_AGENT_NAME", "");
        std::env::set_var("VOX_ROOM_TTS", "");
        // ...and one that is really set, to be sure it survives.
        std::env::set_var("VOX_ROOM_VOICE", "cherry");

        forget_blank_env();
        let cli = Cli::try_parse_from(["vox_room"]).expect("blank VOX_ROOM_TTS must not fail");

        assert_eq!(cli.prompt, None, "an empty prompt is not a prompt");
        assert_eq!(cli.tts_url, None);
        assert!(cli.tts.is_none());
        assert_eq!(cli.name, "Vox", "falls back to the default");
        assert_eq!(cli.voice, "cherry", "a real value is left alone");

        for v in [
            "VOX_ROOM_PROMPT",
            "VOX_ROOM_TTS_URL",
            "VOX_ROOM_AGENT_NAME",
            "VOX_ROOM_TTS",
            "VOX_ROOM_VOICE",
        ] {
            std::env::remove_var(v);
        }
    }
}
