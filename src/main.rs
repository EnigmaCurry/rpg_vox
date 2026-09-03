use anyhow::Result;
use clap::Parser;
use tokio::sync::mpsc;
use tracing::info;

mod http;
mod pw_source;
mod settings;
mod tts;

/// Voice bridge over pipewire.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Address for the local /say HTTP endpoint.
    #[arg(long, default_value = "127.0.0.1:7331")]
    bind: String,

    /// ComfyUI base URL (HTTP; the WebSocket endpoint is derived from this).
    #[arg(long, default_value = "http://127.0.0.1:8188")]
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
    info!(node = %args.node_name, "PipeWire source node started");

    // Everything else lives on the tokio runtime.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let shared_settings = settings::new(settings::Settings {
        comfyui_base: args.comfyui.trim_end_matches('/').to_string(),
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
        ));

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
