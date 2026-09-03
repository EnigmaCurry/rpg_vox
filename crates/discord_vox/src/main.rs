use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result};
use clap::Parser;
use rtrb::{Consumer, RingBuffer};
use serenity::{
    async_trait,
    client::{Client, Context as SerenityContext, EventHandler},
    http::Http,
    model::{
        gateway::Ready,
        id::{ChannelId, GuildId, UserId},
        voice::VoiceState,
    },
    prelude::GatewayIntents,
};
use discortp::{Packet, PacketSize, rtp::RtpExtensionPacket};
use songbird::{
    Config as SongbirdConfig, SerenityInit,
    driver::{DecodeConfig, DecodeMode},
    events::{CoreEvent, Event, EventContext, EventHandler as VoiceEventHandler, TrackEvent},
    input::{Input, RawAdapter, codecs::{get_codec_registry, get_probe}},
    tracks::PlayMode,
};
use symphonia_core::io::MediaSource;
use tokio::sync::Mutex as AsyncMutex;
use tracing::{error, info, warn};

mod pw_sink;
mod recorder;

/// Discord voice bridge over PipeWire.
///
/// Registers an Audio/Sink named `--node-name`. Anything you route into it
/// (Helvum / qpwgraph / pw-link) is streamed as the bot's mic in the joined
/// voice channel. The bot auto-joins the configured channel when a human is
/// present and auto-parts when the channel empties.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Discord bot token. Prefer setting DISCORD_TOKEN in the environment.
    #[arg(long, env = "DISCORD_TOKEN", hide_env_values = true)]
    token: String,

    /// Guild (server) ID containing the target voice channel.
    #[arg(long, env = "DISCORD_GUILD_ID")]
    guild_id: u64,

    /// Voice channel ID to join when occupied.
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

    /// If set, record each join session under this directory. Produces one
    /// `user-<id>.wav` per speaker plus a combined `mixed.wav`, all at
    /// 48 kHz 16-bit stereo. Off by default (recording requires consent —
    /// tell the channel).
    #[arg(long, env = "DISCORD_RECORD_DIR")]
    record: Option<PathBuf>,
}

/// Bridges the SPSC ring buffer (fed by the PipeWire RT callback) into a
/// synchronous byte reader that songbird can consume. On underrun we emit
/// silence rather than EOF so playback stays live indefinitely.
///
/// The consumer is held behind an `Arc<Mutex<...>>` so we can build a fresh
/// `PcmSource` for each auto-join without losing the shared audio stream.
struct PcmSource {
    consumer: Arc<Mutex<Consumer<f32>>>,
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

struct Handler {
    guild_id: GuildId,
    channel_id: ChannelId,
    consumer: Arc<Mutex<Consumer<f32>>>,
    sample_rate: u32,
    channels: u32,
    /// If Some, record each session under this root directory.
    record_root: Option<PathBuf>,
    /// The current session's recorder, if we're in a channel and recording.
    /// Held here so leave() can finalize it before songbird drops the Call.
    active_recorder: Mutex<Option<Arc<recorder::Recorder>>>,
    /// True iff we currently believe we are connected to the voice channel.
    joined: AtomicBool,
    /// Serialises reconcile() so overlapping voice_state_update events can't
    /// race into a double-join or leave-during-join.
    reconcile: AsyncMutex<()>,
}

impl Handler {
    /// Count humans (anyone other than the bot itself) currently in the
    /// target channel, using serenity's voice-state cache.
    fn count_humans(&self, ctx: &SerenityContext) -> Option<usize> {
        let guild = ctx.cache.guild(self.guild_id)?;
        let bot_id = ctx.cache.current_user().id;
        let n = guild
            .voice_states
            .values()
            .filter(|vs| vs.channel_id == Some(self.channel_id) && vs.user_id != bot_id)
            .count();
        Some(n)
    }

    async fn reconcile(&self, ctx: &SerenityContext) {
        let _guard = self.reconcile.lock().await;
        let Some(humans) = self.count_humans(ctx) else {
            // Cache not ready yet — nothing to do; we'll get called again.
            return;
        };
        let joined = self.joined.load(Ordering::SeqCst);
        match (humans > 0, joined) {
            (true, false) => self.join(ctx).await,
            (false, true) => self.leave(ctx).await,
            _ => {}
        }
    }

    async fn join(&self, ctx: &SerenityContext) {
        let manager = match songbird::get(ctx).await {
            Some(m) => m,
            None => {
                error!("songbird not registered on the client");
                return;
            }
        };

        // Grab (or create) the Call *before* joining voice so we can register
        // receive handlers up-front. The initial SpeakingStateUpdate events —
        // which carry the SSRC → UserId mapping for users already in the
        // channel — fire during the join handshake; if the handlers aren't
        // attached yet, we miss those mappings and end up with `ssrc-*.wav`
        // files instead of `user-*.wav`.
        let call = manager.get_or_insert(self.guild_id);

        if let Some(root) = &self.record_root {
            let session_dir = recorder::timestamped_session_dir(root);
            match recorder::Recorder::new(session_dir.clone()) {
                Ok(rec) => {
                    let rec = Arc::new(rec);
                    let mut driver = call.lock().await;
                    driver.remove_all_global_events();
                    let make_receiver = || VoiceReceiver {
                        recorder: Arc::clone(&rec),
                        guild_id: self.guild_id,
                        http: Arc::clone(&ctx.http),
                    };
                    driver.add_global_event(
                        CoreEvent::SpeakingStateUpdate.into(),
                        make_receiver(),
                    );
                    driver.add_global_event(CoreEvent::VoiceTick.into(), make_receiver());
                    drop(driver);
                    *self.active_recorder.lock().unwrap() = Some(rec);
                    info!(dir = %session_dir.display(), "recording session started");
                }
                Err(err) => {
                    error!(?err, "failed to start recording session");
                }
            }
        }

        if let Err(err) = manager.join(self.guild_id, self.channel_id).await {
            error!(?err, "failed to join voice channel");
            if let Some(rec) = self.active_recorder.lock().unwrap().take() {
                rec.finalize();
            }
            let _ = manager.remove(self.guild_id).await;
            return;
        }
        info!(
            guild = self.guild_id.get(),
            channel = self.channel_id.get(),
            "joined voice channel"
        );

        let source = PcmSource {
            consumer: Arc::clone(&self.consumer),
        };
        let raw = RawAdapter::new(source, self.sample_rate, self.channels);
        let input: Input = raw.into();
        let input = match input
            .make_playable_async(&get_codec_registry(), &get_probe())
            .await
        {
            Ok(i) => i,
            Err(err) => {
                error!(?err, "input make_playable failed");
                let _ = manager.remove(self.guild_id).await;
                return;
            }
        };
        let mut driver = call.lock().await;
        let handle = driver.play_input(input);
        for evt in [TrackEvent::Error, TrackEvent::End] {
            if let Err(err) = handle.add_event(Event::Track(evt), TrackDiag) {
                warn!(?evt, ?err, "failed to attach track event handler");
            }
        }
        drop(driver);

        self.joined.store(true, Ordering::SeqCst);
        info!("streaming PipeWire sink to voice channel");
    }

    async fn leave(&self, ctx: &SerenityContext) {
        let manager = match songbird::get(ctx).await {
            Some(m) => m,
            None => {
                error!("songbird not registered on the client");
                return;
            }
        };
        match manager.remove(self.guild_id).await {
            Ok(()) => info!("left voice channel (empty)"),
            Err(err) => warn!(?err, "leave failed"),
        }
        if let Some(rec) = self.active_recorder.lock().unwrap().take() {
            rec.finalize();
            let session_dir = rec.session_dir();
            info!(dir = %session_dir.display(), "recording session finalized");
            drop(rec);
            tokio::spawn(async move {
                recorder::transcode_session(session_dir).await;
            });
        }
        self.joined.store(false, Ordering::SeqCst);
    }
}

/// Fans SpeakingStateUpdate + VoiceTick events into the current session's
/// Recorder. Also kicks off async display-name lookups so per-user WAVs can
/// be renamed from `user-<uid>.wav` to `user-<uid>-<name>.wav`.
struct VoiceReceiver {
    recorder: Arc<recorder::Recorder>,
    guild_id: GuildId,
    http: Arc<Http>,
}

#[async_trait]
impl VoiceEventHandler for VoiceReceiver {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        match ctx {
            EventContext::SpeakingStateUpdate(speaking) => {
                let user_id = speaking.user_id.map(|u| u.0);
                self.recorder.note_speaker(speaking.ssrc, user_id);
                if let Some(uid) = user_id {
                    let http = Arc::clone(&self.http);
                    let recorder = Arc::clone(&self.recorder);
                    let guild_id = self.guild_id;
                    tokio::spawn(async move {
                        if let Some(name) = fetch_display_name(&http, guild_id, uid).await {
                            recorder.note_display_name(uid, &name);
                        }
                    });
                }
            }
            EventContext::VoiceTick(tick) => {
                let pcm: HashMap<u32, &[i16]> = tick
                    .speaking
                    .iter()
                    .filter_map(|(&ssrc, data)| {
                        data.decoded_voice.as_deref().map(|d| (ssrc, d))
                    })
                    .collect();
                // Extracting the raw Opus payload from an RTP packet Discord
                // handed us requires three trims:
                //   1. RTP header (via rtp.payload()).
                //   2. Crypto prefix/suffix (via payload_offset..payload_end_pad).
                //      Note: in the VoiceTick path songbird 0.6 populates
                //      payload_end_pad as an *end index* into rtp.payload(),
                //      not the "trim count" its docstring claims.
                //   3. RTP header extension, if the extension bit is set on
                //      the outer packet. Discord DOES set this bit for
                //      per-user audio, so without stripping it decoders see
                //      [ext_header][opus] and fail with "Error parsing the
                //      packet header".
                let opus: HashMap<u32, Vec<u8>> = tick
                    .speaking
                    .iter()
                    .filter_map(|(&ssrc, data)| {
                        let p = data.packet.as_ref()?;
                        let rtp = p.rtp();
                        let has_ext = rtp.get_extension() != 0;
                        let payload = rtp.payload();
                        let end = p.payload_end_pad.min(payload.len());
                        let mut start = p.payload_offset.min(end);
                        if has_ext && start < end {
                            if let Some(ext) = RtpExtensionPacket::new(&payload[start..end]) {
                                start = (start + ext.packet_size()).min(end);
                            }
                        }
                        (start < end).then(|| (ssrc, payload[start..end].to_vec()))
                    })
                    .collect();
                let opus_refs: HashMap<u32, &[u8]> =
                    opus.iter().map(|(k, v)| (*k, v.as_slice())).collect();
                self.recorder.write_tick(&pcm, &opus_refs);
            }
            _ => {}
        }
        None
    }
}

/// Resolve a Discord user's display name: guild nickname if set, otherwise
/// their global display name, otherwise their username. Sanitized for use in
/// a filename. Returns None on any failure or if nothing usable is left.
async fn fetch_display_name(http: &Http, guild_id: GuildId, user_id: u64) -> Option<String> {
    match http.get_member(guild_id, UserId::new(user_id)).await {
        Ok(member) => {
            let raw = member
                .nick
                .as_deref()
                .or(member.user.global_name.as_deref())
                .unwrap_or(&member.user.name);
            let sanitized = recorder::sanitize_display_name(raw);
            (!sanitized.is_empty()).then_some(sanitized)
        }
        Err(err) => {
            warn!(?err, user_id, "failed to fetch member display name");
            None
        }
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, _ctx: SerenityContext, ready: Ready) {
        info!(user = %ready.user.name, "discord gateway ready");
    }

    async fn cache_ready(&self, ctx: SerenityContext, _guilds: Vec<GuildId>) {
        info!("guild cache ready");
        self.reconcile(&ctx).await;
    }

    async fn voice_state_update(
        &self,
        ctx: SerenityContext,
        old: Option<VoiceState>,
        new: VoiceState,
    ) {
        let bot_id = ctx.cache.current_user().id;
        if new.user_id == bot_id {
            // Our own state changes don't affect occupancy.
            return;
        }
        let touches_target = new.channel_id == Some(self.channel_id)
            || old.as_ref().and_then(|o| o.channel_id) == Some(self.channel_id);
        if !touches_target {
            return;
        }
        self.reconcile(&ctx).await;
    }
}

/// Logs the actual state on Error/End so we can see why a track died.
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
                        info!(position = ?state.position, "track ended");
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

    let recording = args.record.is_some();
    if recording {
        info!(dir = %args.record.as_ref().unwrap().display(), "voice recording enabled");
    }

    let handler = Handler {
        guild_id: GuildId::new(args.guild_id),
        channel_id: ChannelId::new(args.channel_id),
        consumer: Arc::new(Mutex::new(consumer)),
        sample_rate: args.sample_rate,
        channels: args.channels,
        record_root: args.record.clone(),
        active_recorder: Mutex::new(None),
        joined: AtomicBool::new(false),
        reconcile: AsyncMutex::new(()),
    };

    let intents = GatewayIntents::GUILDS | GatewayIntents::GUILD_VOICE_STATES;

    let songbird_config = if recording {
        SongbirdConfig::default().decode_mode(DecodeMode::Decode(DecodeConfig::default()))
    } else {
        SongbirdConfig::default()
    };

    let mut client = Client::builder(&args.token, intents)
        .event_handler(handler)
        .register_songbird_from_config(songbird_config)
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
