use anyhow::{Context as _, Result};
use clap::{Parser, ValueEnum};
use tokio::sync::mpsc;
use tracing::info;

mod chat;
mod http;
mod pw_source;
mod settings;
mod store;
mod tts;
mod workflow;

#[derive(Copy, Clone, Debug, ValueEnum)]
enum TtsBackend {
    Piper,
    Comfyui,
    /// Remote Qwen3-TTS Gradio deployment. Inference runs on the remote GPU;
    /// rpg_vox just fetches the resulting WAV. Configure with --qwen3-*.
    Qwen3,
}

/// Voice bridge over pipewire.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Address for the local /say HTTP endpoint.
    #[arg(long, default_value = "127.0.0.1:7331")]
    bind: String,

    /// TTS backend to run. `piper` is local, in-process, low-latency (default);
    /// `comfyui` submits to a remote ComfyUI instance over HTTP+WS.
    #[arg(long, value_enum, env = "RPG_VOX_TTS_BACKEND", default_value_t = TtsBackend::Piper)]
    tts_backend: TtsBackend,

    /// Path to the Piper ONNX voice model (required when --tts-backend=piper).
    #[arg(
        long,
        env = "RPG_VOX_PIPER_MODEL",
        default_value = "models/piper/en_US-kristin-medium.onnx"
    )]
    piper_model: String,

    /// Path to the Piper voice config JSON. Defaults to `{model}.json`.
    #[arg(long, env = "RPG_VOX_PIPER_CONFIG")]
    piper_config: Option<String>,

    /// Speaker id for multi-speaker Piper voices. Ignored by single-speaker models.
    #[arg(long, env = "RPG_VOX_PIPER_SPEAKER")]
    piper_speaker: Option<i64>,

    /// Minimum words per phrase before the chunker will accept a punctuation
    /// break. Raise this to get longer, more naturally intonated Piper output
    /// at the cost of higher time-to-first-audio.
    #[arg(long, env = "RPG_VOX_PIPER_MIN_WORDS", default_value_t = 5)]
    piper_min_words: usize,

    /// Upper word count at which the chunker force-breaks a run-on phrase
    /// even without punctuation. Raise together with --piper-min-words for
    /// longer chunks; lower for faster first audio.
    #[arg(long, env = "RPG_VOX_PIPER_MAX_WORDS", default_value_t = 20)]
    piper_max_words: usize,

    /// Piper duration multiplier (`length_scale`). Values > 1.0 slow speech
    /// down; < 1.0 speed it up. Unset uses the voice model's own default
    /// (typically 1.0). Try 1.2–1.4 for a noticeably calmer cadence.
    #[arg(long, env = "RPG_VOX_PIPER_LENGTH_SCALE")]
    piper_length_scale: Option<f32>,

    /// Word count at which the chunker will break on a comma / colon /
    /// semicolon. Unset picks a midpoint between min and max, which biases
    /// toward sentence-end breaks. Set to the same value as --piper-min-words
    /// to make commas audibly pause (each comma-terminated segment becomes
    /// its own chunk with an inter-chunk silence).
    #[arg(long, env = "RPG_VOX_PIPER_WEAK_AFTER_WORDS")]
    piper_weak_after_words: Option<usize>,

    /// Silence (ms) inserted after a chunk that ended on `,:;`. Default 140.
    /// Raise this if comma breaks still feel too rushed.
    #[arg(long, env = "RPG_VOX_PIPER_WEAK_MS", default_value_t = 140)]
    piper_weak_ms: u32,

    /// Silence (ms) inserted after a chunk that ended on `.?!`. Default 320.
    #[arg(long, env = "RPG_VOX_PIPER_STRONG_MS", default_value_t = 320)]
    piper_strong_ms: u32,

    /// ComfyUI base URL (HTTP; the WebSocket endpoint is derived from this).
    #[arg(long, env = "RPG_VOX_COMFYUI", default_value = "http://127.0.0.1:8188")]
    comfyui: String,

    /// Base URL of a Qwen3-TTS Gradio deployment (no trailing slash).
    /// Required when --tts-backend=qwen3; set via CLI or RPG_VOX_QWEN3_URL.
    #[arg(long, env = "RPG_VOX_QWEN3_URL")]
    qwen3_url: Option<String>,

    /// Preset speaker on the Qwen3-TTS Gradio app. One of: Serena, Vivian,
    /// Uncle Fu, Ryan, Aiden, Ono Anna, Sohee, Eric, Dylan.
    #[arg(long, env = "RPG_VOX_QWEN3_SPEAKER", default_value = "Ryan")]
    qwen3_speaker: String,

    /// Language for Qwen3-TTS. One of: Auto, Chinese, English, German,
    /// Italian, Portuguese, Spanish, Japanese, Korean, French, Russian.
    #[arg(long, env = "RPG_VOX_QWEN3_LANGUAGE", default_value = "Auto")]
    qwen3_language: String,

    /// Optional voice-style instruction sent to Qwen3-TTS. Empty by default.
    #[arg(long, env = "RPG_VOX_QWEN3_INSTRUCT", default_value = "")]
    qwen3_instruct: String,

    /// Per-request timeout in seconds for the Qwen3-TTS Gradio API.
    #[arg(long, env = "RPG_VOX_QWEN3_TIMEOUT", default_value_t = 600)]
    qwen3_timeout_secs: u64,

    /// User-facing name shown in Firefox's mic picker. Defaults to the
    /// instance name (RPG_VOX_NAME) when that is set, otherwise "RPG Vox".
    #[arg(long)]
    node_description: Option<String>,

    /// Unique instance name — used as the PipeWire `node.name` (short id, no
    /// spaces). Set this when running multiple rpg_vox processes so each is
    /// identifiable in the graph. Defaults to "rpg-vox".
    #[arg(long, env = "RPG_VOX_NAME")]
    node_name: Option<String>,

    /// Sample rate offered to PipeWire (Hz). 48000 is the sane default.
    #[arg(long, default_value_t = 48_000)]
    sample_rate: u32,

    /// Ring buffer capacity in seconds of audio.
    #[arg(long, default_value_t = 4.0)]
    ringbuf_seconds: f32,

    /// Directory of `*.json` ComfyUI workflows to make selectable at runtime.
    /// Each file must be in API format and contain `{{TEXT}}` wherever the
    /// utterance text should go.
    #[arg(long, default_value = "workflows")]
    workflows_dir: String,

    /// Root of the app-managed persistent store. Holds `rpg_vox.sqlite`
    /// (widget metadata) and `clips/{id}.wav` (rendered audio). Created on
    /// first boot if absent.
    #[arg(long, env = "RPG_VOX_DATA_DIR", default_value = "data")]
    data_dir: std::path::PathBuf,

    /// Which workflow (file stem, e.g. "chatterbox") to select at startup.
    /// If unset, the first workflow found alphabetically is used, or the
    /// built-in placeholder if the directory is empty.
    #[arg(long)]
    default_workflow: Option<String>,

    /// Skip the startup warmup submission.
    #[arg(long, default_value_t = false)]
    no_warmup: bool,

    /// PipeWire `node.name` of an Audio/Sink to auto-link this source to on
    /// startup (retries until the sink appears). Handy for pinning rpg-vox
    /// to a companion like `discord-vox` without dragging cables in Helvum.
    #[arg(long, env = "RPG_VOX_AUTO_LINK")]
    auto_link: Option<String>,

    /// OpenAI-compatible chat endpoint base URL (must end in `/v1`).
    #[arg(
        long,
        env = "RPG_VOX_CHAT_BASE_URL",
        default_value = "http://127.0.0.1:8000/v1"
    )]
    chat_base_url: String,

    /// Model name to send in `chat/completions` requests.
    #[arg(long, env = "RPG_VOX_CHAT_MODEL", default_value = "Qwen/Qwen3-8B")]
    chat_model: String,

    /// Bearer token for the chat endpoint (many local vLLM/ollama servers
    /// don't require one).
    #[arg(long, env = "RPG_VOX_CHAT_API_KEY")]
    chat_api_key: Option<String>,

    /// Path to a file containing the system prompt. Missing file is tolerated
    /// (the chat runs with no system prompt) so a fresh checkout without the
    /// default prompts/ dir still boots.
    #[arg(
        long,
        env = "RPG_VOX_CHAT_SYSTEM_PROMPT_FILE",
        default_value = "prompts/default.txt"
    )]
    chat_system_prompt_file: String,

    /// Cap on the number of non-system messages kept in chat history; older
    /// turns are dropped from the LLM request.
    #[arg(long, env = "RPG_VOX_CHAT_MAX_HISTORY", default_value_t = 40)]
    chat_max_history: usize,

    /// Max tokens generated per chat reply.
    #[arg(long, env = "RPG_VOX_CHAT_MAX_TOKENS", default_value_t = 512)]
    chat_max_tokens: u32,

    /// Suppress Qwen3-style reasoning by sending
    /// `chat_template_kwargs.enable_thinking=false` in each request. Default
    /// on since TTS wants the answer, not the scratchpad — and reasoning
    /// easily blows past `--chat-max-tokens` and truncates without ever
    /// producing a reply. Set to `false` to opt back in to reasoning.
    #[arg(
        long,
        env = "RPG_VOX_CHAT_DISABLE_THINKING",
        default_value_t = true,
        action = clap::ArgAction::Set,
    )]
    chat_disable_thinking: bool,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                // ort spams per-node optimizer info at INFO — pin it to WARN so
                // the rpg_vox startup log stays readable.
                tracing_subscriber::EnvFilter::new("info,ort=warn,ort::logging=warn")
            }),
        )
        .init();

    let args = Args::parse();

    let ringbuf_frames = (args.sample_rate as f32 * args.ringbuf_seconds) as usize;
    let (producer, consumer) = rtrb::RingBuffer::<f32>::new(ringbuf_frames);

    // If RPG_VOX_NAME / --node-name is set, derive the mic-picker label from
    // it too so multi-instance setups show distinct entries; otherwise keep
    // the historical "RPG Vox" default.
    let name_explicit = args.node_name.is_some();
    let node_name = args
        .node_name
        .clone()
        .unwrap_or_else(|| "rpg-vox".to_string());
    let node_description = args.node_description.clone().unwrap_or_else(|| {
        if name_explicit {
            node_name.clone()
        } else {
            "RPG Vox".to_string()
        }
    });

    // PipeWire runs its own event loop on a dedicated OS thread.
    let pw_cfg = pw_source::Config {
        node_name: node_name.clone(),
        node_description: node_description.clone(),
        sample_rate: args.sample_rate,
        auto_patch_target: args.auto_link.clone(),
    };
    let pw_handle = pw_source::spawn(pw_cfg, consumer)?;
    let pw_client = pw_handle.client.clone();
    let auto_link_client = pw_client.clone();
    info!(node = %node_name, description = %node_description, "PipeWire source node started");

    // Everything else lives on the tokio runtime.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let registry = std::sync::Arc::new(workflow::Registry::from_dir(
        std::path::Path::new(&args.workflows_dir),
    ));
    info!(
        dir = %args.workflows_dir,
        count = registry.entries().count(),
        "workflow registry loaded"
    );

    let (initial_name, workflow_json, workflow_summary) = {
        let pick = args
            .default_workflow
            .as_deref()
            .or_else(|| registry.first_name());
        match pick.and_then(|n| registry.get(n)) {
            Some(entry) => {
                info!(name = %entry.name, nodes = entry.summary.node_count, "using workflow");
                (
                    Some(entry.name.clone()),
                    entry.json.clone(),
                    entry.summary.clone(),
                )
            }
            None => {
                if args.default_workflow.is_some() {
                    tracing::warn!(
                        wanted = ?args.default_workflow,
                        "requested workflow not in registry; falling back to builtin placeholder"
                    );
                } else if registry.is_empty() {
                    info!("workflow registry empty; using builtin placeholder");
                }
                let (j, s) = workflow::builtin_placeholder();
                (None, j, s)
            }
        }
    };
    if !workflow_summary.has_text_placeholder {
        tracing::warn!(
            "workflow does not contain the `{}` token — utterance text won't reach any node",
            workflow::TEXT_TOKEN
        );
    }

    let shared_settings = settings::new(settings::Settings {
        backend: match args.tts_backend {
            TtsBackend::Piper => "piper",
            TtsBackend::Comfyui => "comfyui",
            TtsBackend::Qwen3 => "qwen3",
        },
        comfyui_base: args.comfyui.trim_end_matches('/').to_string(),
        workflow_name: initial_name,
        workflow_json,
        workflow_summary,
    });

    let system_prompt = match std::fs::read_to_string(&args.chat_system_prompt_file) {
        Ok(text) => {
            let trimmed = text.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!(
                path = %args.chat_system_prompt_file,
                "chat system prompt file not found; chat will run without a system prompt"
            );
            None
        }
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "reading chat system prompt file {}",
                    args.chat_system_prompt_file
                )
            });
        }
    };
    let chat_client = chat::Client::new(chat::Config {
        base_url: args.chat_base_url.trim_end_matches('/').to_string(),
        model: args.chat_model.clone(),
        api_key: args.chat_api_key.clone(),
        system_prompt,
        max_history: args.chat_max_history,
        max_tokens: args.chat_max_tokens,
        disable_thinking: args.chat_disable_thinking,
    });
    info!(
        base = %chat_client.base_url(),
        model = %chat_client.model(),
        "chat client configured"
    );

    let backend = build_backend(&args, shared_settings.clone())?;
    info!(backend = backend.kind(), "tts backend ready");

    let store = store::Store::open(&args.data_dir)
        .with_context(|| format!("opening store at {}", args.data_dir.display()))?;
    info!(data_dir = %args.data_dir.display(), "widget store opened");

    let result = rt.block_on(async move {
        let (tts_tx, tts_rx) = mpsc::channel::<tts::Command>(32);

        let tts_cfg = tts::Config {
            target_sample_rate: args.sample_rate,
        };
        let tts_task = tokio::spawn(tts::run(tts_cfg, backend, tts_rx, producer));

        let http_task = tokio::spawn(http::serve(
            args.bind.clone(),
            tts_tx,
            pw_client,
            shared_settings.clone(),
            registry.clone(),
            chat_client.clone(),
            store,
        ));

        // Verify + warmup are ComfyUI-specific — skip both entirely when a
        // local backend (piper) is active, otherwise the log fills with
        // "connection refused" from probing an endpoint we're not using.
        let use_comfyui = matches!(args.tts_backend, TtsBackend::Comfyui);
        if use_comfyui {
            let verify_settings = shared_settings.clone();
            tokio::spawn(async move {
                let (base, classes) = {
                    let s = verify_settings.read().await;
                    (s.comfyui_base.clone(), s.workflow_summary.node_classes.clone())
                };
                let http = reqwest::Client::new();
                match workflow::missing_nodes(&http, &base, &classes).await {
                    Ok(missing) if missing.is_empty() => {
                        info!("workflow verified: all node classes present on ComfyUI");
                    }
                    Ok(missing) => {
                        tracing::warn!(missing = ?missing, "workflow references nodes not present on ComfyUI");
                    }
                    Err(err) => {
                        tracing::warn!(err = %format!("{err:#}"), "could not verify workflow (is ComfyUI reachable?)");
                    }
                }
            });
        }

        if let Some(target) = args.auto_link.clone() {
            tokio::spawn(auto_link(auto_link_client, target));
        }

        if use_comfyui && !args.no_warmup {
            let warm_settings = shared_settings.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                let (base, wf) = {
                    let s = warm_settings.read().await;
                    (s.comfyui_base.clone(), s.workflow_json.clone())
                };
                match tts::warmup(&base, &wf).await {
                    Ok(()) => info!("warmup succeeded"),
                    Err(err) => tracing::warn!(err = %format!("{err:#}"), "warmup failed"),
                }
            });
        }

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("ctrl-c received, shutting down");
                Ok::<_, anyhow::Error>(())
            }
            r = tts_task => {
                r??;
                Ok(())
            }
            r = http_task => {
                r??;
                Ok(())
            }
        }
    });

    pw_handle.shutdown();
    result
}

fn build_backend(args: &Args, settings: settings::Shared) -> Result<tts::Backend> {
    match args.tts_backend {
        TtsBackend::Piper => {
            let config_path = args
                .piper_config
                .clone()
                .unwrap_or_else(|| format!("{}.json", args.piper_model));
            if args.piper_min_words == 0 || args.piper_max_words < args.piper_min_words {
                return Err(anyhow::anyhow!(
                    "invalid piper chunker sizes: min={} max={} (require 1 <= min <= max)",
                    args.piper_min_words,
                    args.piper_max_words,
                ));
            }
            let default_pauses = tts::piper::PauseConfig::default();
            let cfg = tts::piper::Config {
                model_path: args.piper_model.clone(),
                config_path,
                speaker_id: args.piper_speaker,
                length_scale: args.piper_length_scale,
                chunker: tts::chunker::Config {
                    min_words: args.piper_min_words,
                    max_words: args.piper_max_words,
                    weak_after_words: args.piper_weak_after_words,
                },
                pauses: tts::piper::PauseConfig {
                    strong_ms: args.piper_strong_ms,
                    weak_ms: args.piper_weak_ms,
                    force_ms: default_pauses.force_ms,
                },
            };
            let backend = tts::piper::Backend::load(cfg)
                .context("loading piper backend at startup")?;
            Ok(tts::Backend::Piper(backend))
        }
        TtsBackend::Comfyui => Ok(tts::Backend::Comfy(tts::comfyui::Backend::new(settings))),
        TtsBackend::Qwen3 => {
            let base_url = args.qwen3_url.clone().ok_or_else(|| {
                anyhow::anyhow!(
                    "qwen3-tts backend requires --qwen3-url or RPG_VOX_QWEN3_URL"
                )
            })?;
            let cfg = tts::qwen3::Config {
                base_url,
                speaker: args.qwen3_speaker.clone(),
                language: args.qwen3_language.clone(),
                instruct: args.qwen3_instruct.clone(),
                request_timeout: std::time::Duration::from_secs(args.qwen3_timeout_secs),
            };
            let backend = tts::qwen3::Backend::new(cfg)
                .context("configuring remote qwen3-tts backend")?;
            Ok(tts::Backend::Qwen3(backend))
        }
    }
}

/// Watch the PipeWire graph for an Audio/Sink whose `node.name` matches
/// `target` and (re)patch our source's output to it any time it's not
/// already patched. Runs for the process lifetime so we recover when the
/// peer restarts. Independent of the debug monitor slot.
async fn auto_link(pw: pw_source::PwClient, target: String) {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut had_patch = false;
    loop {
        interval.tick().await;
        let snap = match pw.snapshot().await {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(err = %err, target = %target, "auto-patch snapshot failed");
                continue;
            }
        };
        if snap.own_node_id.is_none() {
            continue;
        }
        if snap.auto_patch_sink_id.is_some() {
            had_patch = true;
            continue;
        }
        let Some(sink) = snap.sinks.iter().find(|s| s.name == target) else {
            if had_patch {
                info!(target = %target, "auto-patch peer gone; awaiting return");
                had_patch = false;
            }
            continue;
        };
        match pw.start_auto_patch(sink.id).await {
            Ok(()) => info!(target = %target, sink_id = sink.id, "auto-patched to sink"),
            Err(err) => {
                tracing::warn!(target = %target, err = %err, "auto-patch failed; will retry");
            }
        }
    }
}
