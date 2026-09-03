use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result};
use clap::Parser;
use rtrb::{Consumer, RingBuffer};
use serenity::{
    async_trait,
    client::{Client, Context as SerenityContext, EventHandler},
    model::{
        gateway::Ready,
        id::{ChannelId, GuildId},
    },
    prelude::GatewayIntents,
};
use songbird::{
    SerenityInit,
    events::{Event, EventContext, EventHandler as VoiceEventHandler, TrackEvent},
    input::{Input, RawAdapter, codecs::{get_codec_registry, get_probe}},
    tracks::PlayMode,
};
use symphonia_core::io::MediaSource;
use tracing::{error, info, warn};

mod pw_sink;

/// Discord voice bridge over PipeWire.
///
/// Registers an Audio/Sink named `--node-name`. Anything you route into it
/// (Helvum / qpwgraph / pw-link) is streamed as the bot's mic in the joined
/// voice channel.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Discord bot token. Prefer setting DISCORD_TOKEN in the environment.
    #[arg(long, env = "DISCORD_TOKEN", hide_env_values = true)]
    token: String,

    /// Guild (server) ID containing the target voice channel.
    #[arg(long, env = "DISCORD_GUILD_ID")]
    guild_id: u64,

    /// Voice channel ID to join on startup.
    #[arg(long, env = "DISCORD_CHANNEL_ID")]
    channel_id: u64,

    /// PipeWire node.name (short id, no spaces).
    #[arg(long, default_value = "discord-vox")]
    node_name: String,

    /// User-facing description shown in routing GUIs.
    #[arg(long, default_value = "Discord Vox")]
    node_description: String,

    /// Sample rate offered to PipeWire (Hz). Discord voice is 48 kHz.
    #[arg(long, default_value_t = 48_000)]
    sample_rate: u32,

    /// Channel count for the sink. Discord supports mono or stereo.
    #[arg(long, default_value_t = 2)]
    channels: u32,

    /// Ring buffer capacity in seconds. Bigger absorbs jitter, smaller
    /// reduces latency. 2s is a sensible default for voice.
    #[arg(long, default_value_t = 2.0)]
    ringbuf_seconds: f32,
}

/// Bridges the SPSC ring buffer (fed by the PipeWire RT callback) into a
/// synchronous byte reader that songbird can consume. On underrun we emit
/// silence rather than EOF so playback stays live indefinitely.
struct PcmSource {
    consumer: Mutex<Consumer<f32>>,
}

impl Read for PcmSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        const SAMPLE_BYTES: usize = std::mem::size_of::<f32>();
        let n_samples = buf.len() / SAMPLE_BYTES;
        if n_samples == 0 {
            return Ok(0);
        }
        let mut consumer = self.consumer.lock().expect("pcm consumer mutex poisoned");
        for i in 0..n_samples {
            let sample = consumer.pop().unwrap_or(0.0);
            buf[i * SAMPLE_BYTES..(i + 1) * SAMPLE_BYTES].copy_from_slice(&sample.to_le_bytes());
        }
        Ok(n_samples * SAMPLE_BYTES)
    }
}

impl Seek for PcmSource {
    fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "live PCM stream is not seekable",
        ))
    }
}

impl MediaSource for PcmSource {
    fn is_seekable(&self) -> bool {
        false
    }
    fn byte_len(&self) -> Option<u64> {
        None
    }
}

/// Held by the serenity `EventHandler`; the `Ready` callback consumes the
/// inner `Option` exactly once to hand ownership to songbird.
struct SharedSource {
    inner: Mutex<Option<PcmSource>>,
    sample_rate: u32,
    channels: u32,
}

struct Handler {
    guild_id: GuildId,
    channel_id: ChannelId,
    source: Arc<SharedSource>,
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: SerenityContext, ready: Ready) {
        info!(user = %ready.user.name, "discord gateway ready");

        let manager = match songbird::get(&ctx).await {
            Some(m) => m,
            None => {
                error!("songbird not registered on the client");
                return;
            }
        };

        let call = match manager.join(self.guild_id, self.channel_id).await {
            Ok(call) => {
                info!(
                    guild = self.guild_id.get(),
                    channel = self.channel_id.get(),
                    "joined voice channel"
                );
                call
            }
            Err(err) => {
                error!(?err, "failed to join voice channel");
                return;
            }
        };

        let pcm = match self.source.inner.lock().expect("shared source mutex").take() {
            Some(p) => p,
            None => {
                // Reconnect / second Ready: source already handed off.
                return;
            }
        };
        let raw = RawAdapter::new(pcm, self.source.sample_rate, self.source.channels);
        let input: Input = raw.into();
        let input = match input
            .make_playable_async(&get_codec_registry(), &get_probe())
            .await
        {
            Ok(i) => {
                info!("input probed and ready");
                i
            }
            Err(err) => {
                error!(?err, "input make_playable failed — songbird/symphonia rejected the raw stream");
                return;
            }
        };
        let mut driver = call.lock().await;
        let handle = driver.play_input(input);
        drop(driver);
        for evt in [TrackEvent::Error, TrackEvent::End, TrackEvent::Playable] {
            if let Err(err) = handle.add_event(Event::Track(evt), TrackDiag) {
                warn!(?evt, ?err, "failed to attach track event handler");
            }
        }
        info!("streaming PipeWire sink to voice channel");
    }
}

/// Logs the actual state on Error/End/Playable so we can see why a track
/// died instead of guessing.
struct TrackDiag;

#[async_trait]
impl VoiceEventHandler for TrackDiag {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let EventContext::Track(states) = ctx {
            for (state, _handle) in *states {
                match &state.playing {
                    PlayMode::Errored(err) => {
                        error!(?err, position = ?state.position, "track errored");
                    }
                    PlayMode::End => {
                        info!(position = ?state.position, play_time = ?state.play_time, "track ended");
                    }
                    other => {
                        info!(state = ?other, "track state changed");
                    }
                }
            }
        }
        None
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    let ring_capacity =
        (args.sample_rate as f32 * args.channels as f32 * args.ringbuf_seconds) as usize;
    let (producer, consumer) = RingBuffer::<f32>::new(ring_capacity);

    let pw_handle = pw_sink::spawn(
        pw_sink::Config {
            node_name: args.node_name.clone(),
            node_description: args.node_description.clone(),
            sample_rate: args.sample_rate,
            channels: args.channels,
        },
        producer,
    )?;
    info!(node = %args.node_name, capacity = ring_capacity, "PipeWire sink node started");

    let frames_in = pw_handle.frames_in.clone();
    let overruns = pw_handle.overruns.clone();
    let frames_logger = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(5));
        ticker.tick().await;
        let mut last = 0u64;
        loop {
            ticker.tick().await;
            let now = frames_in.load(Ordering::Relaxed);
            let delta = now.saturating_sub(last);
            last = now;
            let over = overruns.load(Ordering::Relaxed);
            if delta > 0 || over > 0 {
                info!(frames_5s = delta, total = now, overruns = over, "sink activity");
            }
        }
    });

    let source = Arc::new(SharedSource {
        inner: Mutex::new(Some(PcmSource {
            consumer: Mutex::new(consumer),
        })),
        sample_rate: args.sample_rate,
        channels: args.channels,
    });

    let intents = GatewayIntents::GUILDS | GatewayIntents::GUILD_VOICE_STATES;
    let handler = Handler {
        guild_id: GuildId::new(args.guild_id),
        channel_id: ChannelId::new(args.channel_id),
        source,
    };

    let mut client = Client::builder(&args.token, intents)
        .event_handler(handler)
        .register_songbird()
        .await
        .context("build serenity client")?;

    let shard_manager = Arc::clone(&client.shard_manager);
    tokio::spawn(async move {
        if let Err(err) = tokio::signal::ctrl_c().await {
            error!(?err, "ctrl-c handler failed");
            return;
        }
        info!("ctrl-c received, shutting down");
        shard_manager.shutdown_all().await;
    });

    let start_result = client.start().await;
    frames_logger.abort();
    pw_handle.shutdown();

    if let Err(err) = start_result {
        warn!(?err, "serenity client exited with error");
        return Err(err.into());
    }
    Ok(())
}
