use anyhow::Result;
use clap::Parser;
use tokio::sync::mpsc;
use tracing::info;

mod http;
mod pw_source;
mod settings;
mod tts;
mod workflow;

/// Voice bridge over pipewire.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Address for the local /say HTTP endpoint.
    #[arg(long, default_value = "127.0.0.1:7331")]
    bind: String,

    /// ComfyUI base URL (HTTP; the WebSocket endpoint is derived from this).
    #[arg(long, env = "RPG_VOX_COMFYUI", default_value = "http://127.0.0.1:8188")]
    comfyui: String,

    /// User-facing name shown in Firefox's mic picker.
    #[arg(long, default_value = "RPG Vox")]
    node_description: String,

    /// PipeWire node.name (short id, no spaces).
    #[arg(long, default_value = "rpg-vox")]
    node_name: String,

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
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    let ringbuf_frames = (args.sample_rate as f32 * args.ringbuf_seconds) as usize;
    let (producer, consumer) = rtrb::RingBuffer::<f32>::new(ringbuf_frames);

    // PipeWire runs its own event loop on a dedicated OS thread.
    let pw_cfg = pw_source::Config {
        node_name: args.node_name.clone(),
        node_description: args.node_description.clone(),
        sample_rate: args.sample_rate,
    };
    let pw_handle = pw_source::spawn(pw_cfg, consumer)?;
    let pw_client = pw_handle.client.clone();
    let auto_link_client = pw_client.clone();
    info!(node = %args.node_name, "PipeWire source node started");

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
        comfyui_base: args.comfyui.trim_end_matches('/').to_string(),
        workflow_name: initial_name,
        workflow_json,
        workflow_summary,
    });

    let result = rt.block_on(async move {
        let (say_tx, say_rx) = mpsc::channel::<tts::SayRequest>(32);

        let tts_cfg = tts::Config {
            target_sample_rate: args.sample_rate,
        };
        let tts_task = tokio::spawn(tts::run(
            tts_cfg,
            shared_settings.clone(),
            say_rx,
            producer,
        ));

        let http_task = tokio::spawn(http::serve(
            args.bind.clone(),
            say_tx,
            pw_client,
            shared_settings.clone(),
            registry.clone(),
        ));

        // Verify + warmup run in the background; failures are logged but don't
        // abort startup, since ComfyUI may be started later than us.
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

        if let Some(target) = args.auto_link.clone() {
            tokio::spawn(auto_link(auto_link_client, target));
        }

        if !args.no_warmup {
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

/// Poll the PipeWire graph until an Audio/Sink whose `node.name` matches
/// `target` shows up, then patch our source's output to it. Exits once the
/// auto-patch is established. Independent of the debug monitor slot.
async fn auto_link(pw: pw_source::PwClient, target: String) {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let snap = match pw.snapshot().await {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(err = %err, target = %target, "auto-link snapshot failed");
                continue;
            }
        };
        if snap.own_node_id.is_none() {
            continue;
        }
        if snap.auto_patch_sink_id.is_some() {
            info!(target = %target, "auto-patch already established; watcher exiting");
            return;
        }
        let Some(sink) = snap.sinks.iter().find(|s| s.name == target) else {
            continue;
        };
        match pw.start_auto_patch(sink.id).await {
            Ok(()) => {
                info!(target = %target, sink_id = sink.id, "auto-patched to sink");
                return;
            }
            Err(err) => {
                tracing::warn!(target = %target, err = %err, "auto-patch failed; will retry");
            }
        }
    }
}
