use anyhow::{Context as _, Result};
use clap::{Parser, ValueEnum};
use tokio::sync::mpsc;
use tracing::info;

mod chat;
mod http;
mod mixer;
mod monitor;
mod pw_source;
mod record;
mod script;
mod settings;
mod stderr_filter;
mod store;
mod stt;
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

    /// Base URL of the "presets" Qwen3-TTS Gradio deployment (Qwen3-TTS-
    /// CustomVoice model: 9 named speakers + `instruct` style control).
    /// Required when --tts-backend=qwen3. Legacy alias RPG_VOX_QWEN3_URL
    /// still accepted below for pre-multi-backend .env files.
    #[arg(long, env = "RPG_VOX_QWEN3_PRESETS_URL")]
    qwen3_presets_url: Option<String>,

    /// Legacy single-URL config; treated as the presets URL if the
    /// dedicated var above is unset. Emits a deprecation warning at
    /// startup so users know to migrate.
    #[arg(long, env = "RPG_VOX_QWEN3_URL", hide = true)]
    qwen3_url: Option<String>,

    /// Base URL of the "clone" Qwen3-TTS Gradio deployment (Qwen3-TTS-Base
    /// model: 3-second reference audio → cloned voice). Optional; clone-mode
    /// voice profiles fail at synth if this is unset.
    #[arg(long, env = "RPG_VOX_QWEN3_CLONE_URL")]
    qwen3_clone_url: Option<String>,

    /// Base URL of the "design" Qwen3-TTS Gradio deployment (Qwen3-TTS-
    /// VoiceDesign model: voice generated from a free-text description).
    /// Optional; design-mode voice profiles fail at synth if unset.
    #[arg(long, env = "RPG_VOX_QWEN3_DESIGN_URL")]
    qwen3_design_url: Option<String>,

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

    /// Path to a file containing the system prompt used by the /script page.
    /// The default prompt teaches the LLM the `<speak>…</speak>` convention
    /// so its replies can be split into inline audio widgets. Missing file
    /// is tolerated (the chat runs with no system prompt) so a fresh
    /// checkout without the default prompts/ dir still boots.
    #[arg(
        long,
        env = "RPG_VOX_SCRIPT_SYSTEM_PROMPT_FILE",
        default_value = "prompts/script.txt"
    )]
    script_system_prompt_file: String,

    /// Cap on the number of non-system messages kept in chat history; older
    /// turns are dropped from the LLM request.
    #[arg(long, env = "RPG_VOX_CHAT_MAX_HISTORY", default_value_t = 40)]
    chat_max_history: usize,

    /// Max tokens generated per chat reply. 512 is fine for a direct-answer
    /// model like Qwen3 with thinking disabled; raise for chattier models
    /// (creative-writing / open-thinking) that emit long preambles before
    /// reaching an actual reply.
    #[arg(long, env = "RPG_VOX_CHAT_MAX_TOKENS", default_value_t = 2048)]
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

    /// Path to the SenseVoice ONNX model used to transcribe recordings from
    /// the vox channel. Grab the int8 bundle with `just download-stt-model`
    /// (extracts to `models/sense-voice/`). When unset, recordings still
    /// work — they just won't auto-fill the SpeakCell caption.
    #[arg(
        long,
        env = "RPG_VOX_STT_MODEL",
        default_value = "models/sense-voice/model.int8.onnx"
    )]
    stt_model: std::path::PathBuf,

    /// Tokens file that pairs with `--stt-model`. Same bundle as the model
    /// file — the int8 sense-voice archive ships both side by side.
    #[arg(
        long,
        env = "RPG_VOX_STT_TOKENS",
        default_value = "models/sense-voice/tokens.txt"
    )]
    stt_tokens: std::path::PathBuf,

    /// SenseVoice language hint. `"auto"` lets the model pick; other
    /// accepted values are `en`, `zh`, `ja`, `ko`, `yue`.
    #[arg(long, env = "RPG_VOX_STT_LANGUAGE", default_value = "auto")]
    stt_language: String,

    /// Threads sherpa-onnx uses for feature extraction + inference. Two is
    /// fine for widget-length clips on a desktop; raise for longer captures.
    #[arg(long, env = "RPG_VOX_STT_THREADS", default_value_t = 2)]
    stt_threads: i32,

    /// Disable STT entirely even when model files are present. Handy for
    /// benchmarking, or when the recognizer needs to be bypassed without
    /// deleting the model files.
    #[arg(
        long,
        env = "RPG_VOX_STT_DISABLED",
        default_value_t = false,
        action = clap::ArgAction::Set,
    )]
    stt_disabled: bool,

    /// Streaming Zipformer transducer files — encoder / decoder / joiner /
    /// tokens. Any of the sherpa-onnx streaming Zipformer bundles works;
    /// the English 20M variant is small enough to run comfortably on
    /// a laptop CPU. All four paths must exist for streaming STT to
    /// engage; when any is missing (or --streaming-stt-disabled is set)
    /// the Record page falls back to the offline SenseVoice finalize
    /// path.
    #[arg(
        long,
        env = "RPG_VOX_STREAMING_STT_ENCODER",
        default_value = "models/streaming-zipformer/encoder.onnx"
    )]
    streaming_stt_encoder: std::path::PathBuf,
    #[arg(
        long,
        env = "RPG_VOX_STREAMING_STT_DECODER",
        default_value = "models/streaming-zipformer/decoder.onnx"
    )]
    streaming_stt_decoder: std::path::PathBuf,
    #[arg(
        long,
        env = "RPG_VOX_STREAMING_STT_JOINER",
        default_value = "models/streaming-zipformer/joiner.onnx"
    )]
    streaming_stt_joiner: std::path::PathBuf,
    #[arg(
        long,
        env = "RPG_VOX_STREAMING_STT_TOKENS",
        default_value = "models/streaming-zipformer/tokens.txt"
    )]
    streaming_stt_tokens: std::path::PathBuf,

    #[arg(long, env = "RPG_VOX_STREAMING_STT_THREADS", default_value_t = 2)]
    streaming_stt_threads: i32,

    #[arg(
        long,
        env = "RPG_VOX_STREAMING_STT_DISABLED",
        default_value_t = false,
        action = clap::ArgAction::Set,
    )]
    streaming_stt_disabled: bool,

    /// Number of Vox channels to pre-allocate. Each slot creates its own
    /// pipewire sink (`-vox`, `-vox2`, ...), broadcast tap, mixer strip,
    /// and (when enabled + STT is loaded) VAD/STT worker. Fixed at boot;
    /// changing this restarts the process with a rescaled mixer state.
    #[arg(
        long,
        env = "RPG_VOX_VOX_SLOTS",
        default_value_t = crate::mixer::DEFAULT_VOX_SLOTS
    )]
    vox_slots: usize,
}

fn main() -> Result<()> {
    // Filter sherpa-onnx's "Creating a resampler:" banner (fprintf'd
    // directly to stderr by the C++ library, once per offline decode) out
    // of stderr before tracing grabs it, so the terminal stays readable
    // during recording. Returns the pre-swap TTY state so ANSI colors
    // still get enabled even though the post-swap fd 2 is a pipe.
    let stderr_was_tty = stderr_filter::install();

    // Layered subscriber: the same fmt() logger as before, PLUS an in-memory
    // ring-buffer profiler that captures every `render` span tree from the
    // TTS pipeline (served under /perf/renders). The env-filter only gates
    // fmt output — perf sees every span regardless of RUST_LOG so a
    // production `warn`-level filter still gets us clean render traces.
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        // ort spams per-node optimizer info at INFO — pin it to WARN so
        // the rpg_vox startup log stays readable.
        tracing_subscriber::EnvFilter::new("info,ort=warn,ort::logging=warn")
    });
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(stderr_was_tty)
                .with_filter(env_filter),
        )
        .with(crate::tts::perf::RingLayer::new())
        .init();

    let args = Args::parse();

    let ringbuf_frames = (args.sample_rate as f32 * args.ringbuf_seconds) as usize;
    // Stereo frames end-to-end: rings carry `[L, R]` pairs so the source
    // callback can preserve incoming stereo image (or apply stereo FX in
    // the future) without ever collapsing to mono. TTS is currently mono,
    // and is duplicated to L=R at the ring boundary in `tts::Sink`.
    let (producer, consumer) = rtrb::RingBuffer::<[f32; 2]>::new(ringbuf_frames);

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

    // One broadcast tap per Vox slot. Keepalive receivers hold the
    // channels open when the record worker isn't subscribed yet (or when
    // slots are disabled) so the pw thread's send stays a cheap no-op
    // rather than a Closed error.
    let vox_slot_count = args.vox_slots.max(1);
    let mut vox_taps: Vec<tokio::sync::broadcast::Sender<std::sync::Arc<[f32]>>> =
        Vec::with_capacity(vox_slot_count);
    let mut _vox_keepalives: Vec<tokio::sync::broadcast::Receiver<std::sync::Arc<[f32]>>> =
        Vec::with_capacity(vox_slot_count);
    for _ in 0..vox_slot_count {
        let (tx, rx) = tokio::sync::broadcast::channel::<std::sync::Arc<[f32]>>(32);
        vox_taps.push(tx);
        _vox_keepalives.push(rx);
    }

    // Browser-monitor tap. Fired by the pw source callback with the mixed
    // (TTS + music-input) mono samples about to be handed to pipewire, so
    // `/monitor.ws` subscribers hear what Discord hears. Split off up here
    // so both the pw thread and the HTTP handlers get a handle before the
    // tokio runtime starts.
    let (monitor_tap, _monitor_keepalive) = monitor::channel();

    // Runtime mixer state. Booted with defaults so the pw thread has
    // something to read; persisted state is folded in as a patch below
    // once the store is open.
    let mixer = mixer::AtomicMixer::new(mixer::MixerState::with_slot_count(vox_slot_count));

    // PipeWire runs its own event loop on a dedicated OS thread.
    let pw_cfg = pw_source::Config {
        node_name: node_name.clone(),
        node_description: node_description.clone(),
        sample_rate: args.sample_rate,
        auto_patch_target: args.auto_link.clone(),
        input_taps: vox_taps.clone(),
        monitor_tap: monitor_tap.clone(),
        mixer: mixer.clone(),
    };
    let pw_handle = pw_source::spawn(pw_cfg, consumer)?;
    let pw_client = pw_handle.client.clone();
    let auto_link_client = pw_client.clone();
    let device_routing_client = pw_client.clone();
    info!(
        node = %node_name,
        description = %node_description,
        vox_slots = vox_slot_count,
        "PipeWire source node started (companion sinks: {n}-music, {n}-vox[1..N])",
        n = node_name,
    );

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
        node_name: node_name.clone(),
    });

    let system_prompt = match std::fs::read_to_string(&args.script_system_prompt_file) {
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
                path = %args.script_system_prompt_file,
                "script system prompt file not found; script will run without a system prompt"
            );
            None
        }
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "reading script system prompt file {}",
                    args.script_system_prompt_file
                )
            });
        }
    };
    let chat_client = chat::Client::new(chat::Config {
        base_url: args.chat_base_url.trim_end_matches('/').to_string(),
        model: args.chat_model.clone(),
        api_key: args.chat_api_key.clone(),
        system_prompt: system_prompt.clone(),
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

    // Make sure the single MVP script row exists so /script handlers never
    // hit a foreign-key error on the first user turn of a fresh install.
    rt.block_on(store.ensure_default_script())
        .context("ensuring default script row")?;

    // Seed the built-in "Default" agent from the script prompt file if the
    // agents table doesn't already carry it. The seed happens once — later
    // startups won't clobber user edits to Default (if we ever allow them;
    // for now Default is read-only in the API but future-proof this here).
    let default_agent_prompt = system_prompt.clone().unwrap_or_default();
    rt.block_on(store.ensure_default_agent(default_agent_prompt))
        .context("ensuring default agent row")?;

    // Speech-to-text. Loaded once at startup; missing model files just
    // disable the feature (the record flow still saves the WAV, it simply
    // doesn't auto-fill the SpeakCell caption). --stt-disabled forces None
    // even when the files exist, which is easier than moving the files
    // around while benchmarking.
    let stt_cfg = stt::SttConfig {
        model: (!args.stt_disabled && args.stt_model.exists())
            .then(|| args.stt_model.clone()),
        tokens: (!args.stt_disabled && args.stt_tokens.exists())
            .then(|| args.stt_tokens.clone()),
        language: args.stt_language.clone(),
        num_threads: args.stt_threads,
    };
    if args.stt_disabled {
        info!("STT disabled via --stt-disabled");
    } else if !args.stt_model.exists() {
        info!(
            model = %args.stt_model.display(),
            "STT model file missing — recording caption won't be auto-filled"
        );
    }
    let stt = stt::open_or_warn(&stt_cfg);

    // Streaming Zipformer for the Record page's live transcript. All four
    // files must exist to engage; a missing set drops back to the offline
    // SenseVoice finalize path silently (info-logged for troubleshooting).
    let streaming_all_present = args.streaming_stt_encoder.is_file()
        && args.streaming_stt_decoder.is_file()
        && args.streaming_stt_joiner.is_file()
        && args.streaming_stt_tokens.is_file();
    let streaming_cfg = stt::StreamingSttConfig {
        encoder: (!args.streaming_stt_disabled && streaming_all_present)
            .then(|| args.streaming_stt_encoder.clone()),
        decoder: (!args.streaming_stt_disabled && streaming_all_present)
            .then(|| args.streaming_stt_decoder.clone()),
        joiner: (!args.streaming_stt_disabled && streaming_all_present)
            .then(|| args.streaming_stt_joiner.clone()),
        tokens: (!args.streaming_stt_disabled && streaming_all_present)
            .then(|| args.streaming_stt_tokens.clone()),
        num_threads: args.streaming_stt_threads,
    };
    if args.streaming_stt_disabled {
        info!("streaming STT disabled via --streaming-stt-disabled");
    } else if !streaming_all_present {
        info!(
            encoder = %args.streaming_stt_encoder.display(),
            "streaming STT model bundle incomplete — Record page will use offline SenseVoice"
        );
    }
    let streaming_stt = stt::open_streaming_or_warn(&streaming_cfg);

    // Shared live-transcription state for the Record tab. Channel names
    // come from the mixer's per-slot state now (persisted alongside gain
    // / pan / mute), so RecordState just needs a handle to the mixer.
    let record_state = record::RecordState::new(args.sample_rate, mixer.clone());

    // Restore persisted mixer state before the HTTP server starts serving,
    // so the first GET already reflects what the user had last session.
    // Migrate the legacy single-vox JSON shape and rescale the slot array
    // if the operator changed RPG_VOX_VOX_SLOTS since the last boot.
    match rt.block_on(store.get_mixer()) {
        Ok(Some(mut state)) => {
            state.migrate();
            state.resize_for(vox_slot_count);
            mixer.apply(mixer::MixerPatch::from(state));
            info!("mixer state restored from store");
        }
        Ok(None) => {}
        Err(err) => {
            tracing::warn!(err = %format!("{err:#}"), "reading persisted mixer state failed");
        }
    }

    // Load persisted device routings (mic → vox2, etc.) before the reconciler
    // task starts so the first pass already sees the pins the user set last
    // session. Empty map on error; a garbled row shouldn't block boot.
    let device_routings = std::sync::Arc::new(http::DeviceRoutings::new(
        rt.block_on(store.get_device_routings())
            .unwrap_or_else(|err| {
                tracing::warn!(err = %format!("{err:#}"), "reading persisted device routings failed");
                Default::default()
            }),
    ));
    info!(
        pins = device_routings.lock().unwrap().len(),
        "device routings restored from store"
    );

    let result = rt.block_on(async move {
        let (tts_tx, tts_rx) = mpsc::channel::<tts::Command>(32);

        let tts_cfg = tts::Config {
            target_sample_rate: args.sample_rate,
            ringbuf_frames,
        };
        let tts_task = tokio::spawn(tts::run(tts_cfg, backend, tts_rx, producer, mixer.clone()));

        // Spawn one streaming STT worker per Vox slot before the HTTP
        // server starts serving so early clients see transcripts as soon
        // as `/record` responds. `spawn_worker` no-ops when `stt` is
        // `None`. Each worker checks the slot's `enabled` flag on every
        // chunk so disabled slots stay silent without a per-slot task
        // lifecycle to manage.
        record::spawn_worker(
            stt.clone(),
            streaming_stt.clone(),
            vox_taps.clone(),
            args.sample_rate,
            record_state.clone(),
        );

        // Captures the mixed mic feed into every in-flight recording's
        // silence-gated mixed track. Runs continuously — the worker
        // short-circuits when nothing's being recorded so idle mode is
        // free.
        record::spawn_mixed_worker(monitor_tap.clone(), record_state.clone());

        let http_task = tokio::spawn(http::serve(
            args.bind.clone(),
            tts_tx,
            pw_client,
            shared_settings.clone(),
            registry.clone(),
            chat_client.clone(),
            store,
            monitor_tap,
            args.sample_rate,
            mixer.clone(),
            vox_taps,
            args.sample_rate,
            stt,
            record_state,
            device_routings.clone(),
        ));

        // Reconcile persisted device pins against the live pw graph. Handles
        // three cases uniformly: first-boot restore (device present at boot),
        // hot-plug (mic unplugged and later reattached), and external
        // interference (someone routed a pinned device somewhere else with
        // pactl). See `reconcile_device_routings` for the polling shape.
        tokio::spawn(reconcile_device_routings(
            device_routing_client,
            device_routings.clone(),
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
            // Presets URL falls back to the legacy RPG_VOX_QWEN3_URL so
            // existing .env files keep booting. Emit a deprecation nudge
            // if we hit that path so users know to migrate.
            let presets_url = match (&args.qwen3_presets_url, &args.qwen3_url) {
                (Some(u), _) => u.clone(),
                (None, Some(u)) => {
                    tracing::warn!(
                        "RPG_VOX_QWEN3_URL is deprecated; set RPG_VOX_QWEN3_PRESETS_URL instead"
                    );
                    u.clone()
                }
                (None, None) => {
                    return Err(anyhow::anyhow!(
                        "qwen3-tts backend requires --qwen3-presets-url or RPG_VOX_QWEN3_PRESETS_URL"
                    ));
                }
            };
            let cfg = tts::qwen3::Config {
                presets_url,
                clone_url: args.qwen3_clone_url.clone(),
                design_url: args.qwen3_design_url.clone(),
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

/// Reconcile persisted hardware-input pins against the live PipeWire
/// graph. For each visible `Audio/Source` device whose `node.name` is in
/// the persisted map, re-issue `link_source` if the current routing
/// doesn't already match. Runs for the process lifetime on the same
/// ~500ms cadence as [`auto_link`] so hot-plug and external interference
/// are self-healing without a per-device task lifecycle.
///
/// The map is cloned per tick (it's tiny) so the mutex is never held
/// across the pw round-trip. A missing device is just skipped — it'll be
/// picked up on the tick after it reappears.
async fn reconcile_device_routings(
    pw: pw_source::PwClient,
    prefs: std::sync::Arc<http::DeviceRoutings>,
) {
    use pw_source::SinkRole;
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let pins = {
            let m = prefs.lock().expect("device_routings mutex poisoned");
            if m.is_empty() {
                continue;
            }
            m.clone()
        };
        let snap = match pw.snapshot().await {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(err = %err, "device-routing reconciler snapshot failed");
                continue;
            }
        };
        for source in &snap.sources {
            if source.kind != "device" {
                continue;
            }
            let Some(want) = pins.get(&source.name) else {
                continue;
            };
            // Already routed to the desired target — nothing to do. This
            // is the common case after the initial link lands.
            if source.routed_to.as_deref() == Some(want.as_str()) {
                continue;
            }
            let Some(role) = SinkRole::from_str(want) else {
                tracing::warn!(target = %want, device = %source.name, "invalid saved routing target — skipping");
                continue;
            };
            match pw.link_source(source.id, role).await {
                Ok(()) => info!(device = %source.name, target = %want, "restored device routing"),
                Err(err) => tracing::warn!(
                    device = %source.name,
                    target = %want,
                    err = %err,
                    "restoring device routing failed; will retry"
                ),
            }
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
