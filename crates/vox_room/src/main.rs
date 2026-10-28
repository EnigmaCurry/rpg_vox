//! `vox_room`: a voice room in the browser. People log in with an invite
//! link, talk through their headsets, hear each other and an AI that
//! talks back, and share one live transcript. See ROOM.md.

mod agent;
mod mixer;
mod room;
mod store;
mod stt;
mod tts;
mod web;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context as _, Result};
use clap::{Parser, Subcommand, ValueEnum};
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
    /// Let a reasoning model think before it answers (never spoken).
    #[arg(long, env = "VOX_AGENT_THINK")]
    think: bool,
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
    /// Qwen3 voice.
    #[arg(long, env = "VOX_ROOM_VOICE", default_value = "aiden")]
    voice: String,
    /// How the Qwen3 voice should speak, e.g. "calm and warm".
    #[arg(long, env = "VOX_ROOM_INSTRUCT", default_value = "")]
    instruct: String,
    #[arg(long, env = "VOX_ROOM_LANGUAGE", default_value = "English")]
    language: String,
    /// Kokoro voice.
    #[arg(long, env = "VOX_ROOM_KOKORO_VOICE", default_value = "am_michael")]
    kokoro_voice: String,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print the invite link for NAME, making one if needed.
    Invite { name: String },
    /// Revoke NAME's invite link.
    Uninvite { name: String },
    /// List everyone invited, with their links.
    People,
}

#[derive(Clone, Copy, ValueEnum, PartialEq)]
enum TtsKind {
    Qwen3,
    Kokoro,
    None,
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
    let cli = Cli::parse();
    let db = data_dir(&cli).join("room.db");
    let store = Store::open(&db)?;
    match &cli.command {
        Some(Cmd::Invite { name }) => {
            let name = name.trim();
            if name.is_empty() || name.starts_with('@') {
                bail!("give the person a name");
            }
            println!("{}/join/{}", base_url(&cli), store.invite(name)?);
            return Ok(());
        }
        Some(Cmd::Uninvite { name }) => {
            if !store.uninvite(name)? {
                bail!("{name} wasn't invited");
            }
            println!(
                "{name}'s link no longer works (an open tab stays connected until it reloads)"
            );
            return Ok(());
        }
        Some(Cmd::People) => {
            for (name, token) in store.people()? {
                println!("{name}\t{}/join/{token}", base_url(&cli));
            }
            return Ok(());
        }
        None => {}
    }
    serve(cli, store)
}

fn serve(cli: Cli, store: Store) -> Result<()> {
    if store.people()?.is_empty() {
        warn!("nobody is invited yet: run `vox_room invite NAME` for a link");
    }
    let llm = if cli.echo {
        agent::Llm::Echo
    } else {
        let Some(model) = cli.model.clone() else {
            bail!("set VOX_SCRIBE_LLM_MODEL (or --model), or pass --echo to try the room without a model");
        };
        let endpoint = vox_transcribe::openai::Endpoint::at(cli.llm_url.clone());
        agent::Llm::Chat {
            url: endpoint.url,
            model,
            key: endpoint.api_key,
            think: cli.think,
        }
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
    let voice: Box<dyn tts::Voice> = match kind {
        TtsKind::Qwen3 => {
            let Some(url) = tts_url else {
                bail!("--tts qwen3 needs --tts-url (or VOX_ROOM_TTS_URL)");
            };
            info!(%url, voice = %cli.voice, "voice: Qwen3-TTS");
            Box::new(tts::Qwen3::new(
                &url,
                &cli.voice,
                &cli.instruct,
                &cli.language,
            ))
        }
        TtsKind::Kokoro => {
            info!(voice = %cli.kokoro_voice, "voice: Kokoro");
            Box::new(tts::Kokoro::load(&kokoro_dir, &cli.kokoro_voice)?)
        }
        TtsKind::None => {
            warn!("no voice: the AI's replies are shown, silently, at reading pace");
            Box::new(tts::Silent)
        }
    };

    let factory = if cli.no_stt {
        None
    } else {
        Some(stt::Models::load(&models_dir)?.factory())
    };

    let generation = Arc::new(AtomicU64::new(0));
    let (turn_tx, turn_rx) = crossbeam_channel::unbounded();
    let cfg = RoomConfig {
        agent_name: cli.name.clone(),
        aliases: cli
            .aliases
            .iter()
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty())
            .collect(),
        reply_after: Duration::from_millis(cli.reply_after_ms),
        confirm: Duration::from_millis(cli.confirm_ms),
        history: cli.history,
    };
    let room = Room::new(store, cfg, generation.clone(), turn_tx, factory)?;
    room.mixer.spawn();
    agent::Agent {
        llm,
        prompt,
        generation,
    }
    .spawn(room.clone(), voice, turn_rx);

    let r = room.clone();
    std::thread::Builder::new()
        .name("vox-room-tick".into())
        .spawn(move || {
            let mut meters = false;
            loop {
                r.tick(Instant::now());
                // Meters at 10 per second.
                meters = !meters;
                if meters {
                    r.emit_levels();
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        })
        .context("spawn tick thread")?;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(cli.listen)
            .await
            .with_context(|| format!("listen on {}", cli.listen))?;
        info!(
            "room open on http://{} (invite people with `vox_room invite NAME`)",
            cli.listen
        );
        axum::serve(listener, web::router(room))
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
        anyhow::Ok(())
    })
}
