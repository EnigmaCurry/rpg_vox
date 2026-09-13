use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use clap::Parser;
use discortp::{Packet, PacketSize, rtp::RtpExtensionPacket};
use rtrb::{Consumer, RingBuffer};
use serenity::{
    async_trait,
    client::{Client, Context as SerenityContext, EventHandler},
    http::Http,
    model::{
        gateway::Ready,
        id::{ChannelId, GuildId, MessageId, UserId},
        voice::VoiceState,
    },
    prelude::GatewayIntents,
};
use songbird::{
    Config as SongbirdConfig, SerenityInit,
    driver::{DecodeConfig, DecodeMode},
    events::{CoreEvent, Event, EventContext, EventHandler as VoiceEventHandler, TrackEvent},
    input::{
        Input, RawAdapter,
        codecs::{get_codec_registry, get_probe},
    },
    tracks::PlayMode,
};
use symphonia_core::io::MediaSource;
use tokio::sync::Mutex as AsyncMutex;
use tracing::{error, info, warn};

mod metadata;
mod pw_output;
mod pw_sink;
mod recorder;

/// Read `DISCORD_VOX_AUDIO_OUTPUT_<N>` env vars (N ≥ 1) and return the
/// list of user ids to route to dedicated PipeWire source nodes, ordered
/// by the numeric suffix so slot 1 → first, slot 2 → second, etc.
/// Invalid or non-numeric values are silently skipped.
fn scan_audio_outputs() -> Vec<u64> {
    let mut entries: Vec<(u32, u64)> = std::env::vars()
        .filter_map(|(k, v)| {
            let suffix = k.strip_prefix("DISCORD_VOX_AUDIO_OUTPUT_")?;
            let idx: u32 = suffix.parse().ok()?;
            let uid: u64 = v.trim().parse().ok()?;
            Some((idx, uid))
        })
        .collect();
    entries.sort_by_key(|(i, _)| *i);
    entries.into_iter().map(|(_, uid)| uid).collect()
}

/// Discord voice bridge over PipeWire.
///
/// Registers an Audio/Sink named `--node-name`. Anything you route into it
/// (Helvum / qpwgraph / pw-link) is streamed as the bot's mic in the joined
/// voice channel. The bot auto-joins the configured channel when a human is
/// present and auto-parts when the channel empties.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Discord bot token. Prefer setting DISCORD_VOX_TOKEN in the environment.
    #[arg(long, env = "DISCORD_VOX_TOKEN", hide_env_values = true)]
    token: String,

    /// Guild (server) ID containing the target voice channel.
    #[arg(long, env = "DISCORD_VOX_GUILD_ID")]
    guild_id: u64,

    /// Voice channel ID to join when occupied.
    #[arg(long, env = "DISCORD_VOX_CHANNEL_ID")]
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
    #[arg(long, env = "DISCORD_VOX_RECORD_DIR")]
    record: Option<PathBuf>,

    /// Operator's real name — displayed in the recording-notice message the
    /// bot posts to text-in-voice on each recorded join.
    #[arg(long, env = "DISCORD_VOX_OPERATOR_NAME")]
    operator_name: Option<String>,

    /// Operator's Discord handle (e.g. `enigmacurry`) — displayed in the
    /// recording-notice message.
    #[arg(long, env = "DISCORD_VOX_OPERATOR_DISCORD")]
    operator_discord: Option<String>,

    /// Operator's contact email — displayed in the recording-notice message.
    #[arg(long, env = "DISCORD_VOX_OPERATOR_EMAIL")]
    operator_email: Option<String>,

    /// Path to a SQLite database where the bot upserts Discord metadata it
    /// observes (guild / channel / user names). Defaults to
    /// `<record>/metadata.db` when `--record` is set; otherwise off.
    /// Queryable with `sqlite3`; a proper API is planned.
    #[arg(long, env = "DISCORD_VOX_METADATA_DB")]
    metadata_db: Option<PathBuf>,

    /// Base URL of an rpg_vox instance to feed speaker-attribution hints
    /// to. When set, discord_vox fires `POST {url}/record/hint` on every
    /// Discord speaking event so rpg_vox can attribute VAD-detected
    /// utterances on a pre-mixed vox channel to a specific Discord user.
    /// Leave unset to disable hint sending entirely (feature is opt-in
    /// — rpg_vox is an optional peer).
    #[arg(long, env = "DISCORD_VOX_RPG_VOX_URL")]
    rpg_vox_url: Option<String>,

    /// Optional Vox channel selector to include in each hint (one of
    /// `vox`, `vox2`, ..., matching rpg_vox's pipewire node suffixes).
    /// Omit when the bot's mic feed is routed into whatever vox channel
    /// the user happened to pick — rpg_vox then treats the hint as
    /// applying to any slot.
    #[arg(long, env = "DISCORD_VOX_RPG_VOX_CHANNEL")]
    rpg_vox_channel: Option<String>,

    /// Per-user throttle in milliseconds for outbound hints. VoiceTick
    /// fires every 20 ms; without a throttle we'd hammer rpg_vox with
    /// 50 identical hints/second per active speaker. 500 ms keeps the
    /// ring fresh (rpg_vox's finalize lookup window is 5 s) without
    /// being noisy.
    #[arg(long, env = "DISCORD_VOX_RPG_VOX_HINT_INTERVAL_MS", default_value_t = 500)]
    rpg_vox_hint_interval_ms: u64,
}

#[derive(Clone, Debug, Default)]
struct Operator {
    name: Option<String>,
    discord: Option<String>,
    email: Option<String>,
}

impl Operator {
    fn contact_line(&self) -> Option<String> {
        match (&self.discord, &self.email) {
            (Some(d), Some(e)) => Some(format!(
                "Address all concerns to `{d}` on Discord or to <{e}>."
            )),
            (Some(d), None) => Some(format!("Address all concerns to `{d}` on Discord.")),
            (None, Some(e)) => Some(format!("Address all concerns to <{e}>.")),
            (None, None) => None,
        }
    }
}

fn format_duration(d: chrono::Duration) -> String {
    let secs = d.num_seconds().max(0);
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{h}:{m:02}:{s:02}")
}

/// Companion to the start notice: posted when the recording session ends,
/// with start/end timestamps, elapsed duration, and the display names of
/// every speaker seen. Bot deletes the start WARNING after this posts, so
/// the channel history ends up with only this summary.
fn build_recording_stop_notice(
    started_at: chrono::DateTime<chrono::Local>,
    ended_at: chrono::DateTime<chrono::Local>,
    participants: &[String],
) -> String {
    let elapsed = ended_at.signed_duration_since(started_at);
    let participants_line = if participants.is_empty() {
        "(none)".to_string()
    } else {
        participants.join(", ")
    };
    format!(
        "Recording commenced at {start}. Recording finished at {end}. \
         Duration: {dur}. Participants: {participants_line}",
        start = started_at.format("%Y-%m-%d %H:%M:%S %Z"),
        end = ended_at.format("%Y-%m-%d %H:%M:%S %Z"),
        dur = format_duration(elapsed),
    )
}

/// Consent-notice message posted to the voice channel's text chat when the
/// bot begins a recording session. Includes the local timestamp so the
/// exact start of the recording is on-record in-channel.
fn build_recording_notice(op: &Operator) -> String {
    let mut msg = String::new();
    msg.push_str("**WARNING: This bot may record your audio conversations.**\n\n");
    msg.push_str(
        "These recordings are used only for the game experience. Your voice may \
        be transcribed and processed by a machine that the operator runs locally \
        in his domain.\n",
    );
    if let Some(name) = &op.name {
        msg.push_str(&format!("\nOperator: **{name}**\n"));
    }
    if let Some(contact) = op.contact_line() {
        msg.push('\n');
        msg.push_str(&contact);
        msg.push('\n');
    }
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z");
    msg.push_str(&format!("\n*Recording started at {ts}.*"));
    msg
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

/// Sends "who's talking" hints to a peer rpg_vox instance so it can
/// attribute VAD-detected utterances on a pre-mixed vox channel back
/// to a specific Discord user.
///
/// The client shares its `display_names` cache with the existing
/// SpeakingStateUpdate → fetch_display_name flow — that path already
/// resolves the guild nickname / global name for each SSRC it sees, so
/// hooking in there gives us a free per-user name lookup with the same
/// invalidation semantics (updated whenever Discord fires a fresh
/// speaking-state for a user).
///
/// Per-user throttled so a 20 ms VoiceTick loop doesn't hammer rpg_vox
/// with 50 hints/second per active speaker. rpg_vox's finalize lookup
/// window (5 s) means even a slow cadence keeps attribution accurate.
struct HintClient {
    http: reqwest::Client,
    endpoint: String,
    channel: Option<String>,
    interval: Duration,
    /// Last successful hint dispatch per user id — throttle gate.
    last_sent: Mutex<HashMap<u64, Instant>>,
    /// UserId → resolved display name. Populated by the SpeakingStateUpdate
    /// path's async fetch; consulted on every VoiceTick before sending.
    display_names: Arc<Mutex<HashMap<u64, String>>>,
}

impl HintClient {
    /// Build the client from CLI config. Returns `None` when the rpg_vox
    /// URL is unset (feature disabled). Trailing slashes on the base URL
    /// are trimmed so `http://host:7331` and `http://host:7331/` both work.
    fn from_args(
        rpg_vox_url: Option<&str>,
        channel: Option<&str>,
        interval_ms: u64,
    ) -> Option<Arc<Self>> {
        let base = rpg_vox_url?.trim_end_matches('/').to_string();
        let endpoint = format!("{base}/record/hint");
        // Short connect + total timeout — hint delivery is a nicety, not
        // a critical path, and we don't want a hung POST to leak tasks.
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .ok()?;
        Some(Arc::new(Self {
            http,
            endpoint,
            channel: channel.map(str::to_string),
            interval: Duration::from_millis(interval_ms),
            last_sent: Mutex::new(HashMap::new()),
            display_names: Arc::new(Mutex::new(HashMap::new())),
        }))
    }

    /// Cache a freshly-resolved display name so subsequent hints for
    /// this user id can include it. Called from the same async task
    /// that populates the recorder's display-name map.
    fn note_display_name(&self, uid: u64, name: &str) {
        self.display_names
            .lock()
            .unwrap()
            .insert(uid, name.to_string());
    }

    /// Dispatch a hint for `uid` if the throttle window has elapsed
    /// and we have a display name cached. Silently skips otherwise —
    /// a missing name usually means the SpeakingStateUpdate fetch is
    /// still in flight; a following VoiceTick will pick it up.
    ///
    /// Fire-and-forget: the actual POST runs on a detached task so
    /// VoiceTick's 20 ms cadence isn't gated on network latency.
    fn maybe_send(&self, uid: u64) {
        let now = Instant::now();
        {
            let mut ls = self.last_sent.lock().unwrap();
            if let Some(t) = ls.get(&uid) {
                if now.duration_since(*t) < self.interval {
                    return;
                }
            }
            ls.insert(uid, now);
        }
        let speaker = match self.display_names.lock().unwrap().get(&uid).cloned() {
            Some(name) => name,
            None => return,
        };
        let http = self.http.clone();
        let endpoint = self.endpoint.clone();
        let channel = self.channel.clone();
        tokio::spawn(async move {
            let body = match channel {
                Some(ch) => serde_json::json!({ "speaker": speaker, "channel": ch }),
                None => serde_json::json!({ "speaker": speaker }),
            };
            match http.post(&endpoint).json(&body).send().await {
                Ok(r) if r.status().is_success() => {}
                Ok(r) => {
                    tracing::debug!(status = %r.status(), "rpg_vox rejected speaker hint");
                }
                Err(err) => {
                    tracing::debug!(err = %err, "rpg_vox hint POST failed");
                }
            }
        });
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
    /// Operator details, embedded in the recording-notice message.
    operator: Operator,
    /// Optional metadata sink for guild/channel/user identifiers we observe.
    metadata: Option<Arc<metadata::MetadataDb>>,
    /// The current session's recorder, if we're in a channel and recording.
    /// Held here so leave() can finalize it before songbird drops the Call.
    active_recorder: Mutex<Option<Arc<recorder::Recorder>>>,
    /// Message id of the recording-start WARNING, if we posted one this
    /// session. Cleared by leave() after the WARNING is deleted so the
    /// channel history retains only the stop notice.
    notice_message: Mutex<Option<MessageId>>,
    /// Per-user PipeWire output producers keyed by Discord user id. Empty
    /// when no `DISCORD_VOX_AUDIO_OUTPUT_*` env vars are set. Populated at
    /// startup; the underlying PW source nodes are always running.
    audio_outputs: Arc<HashMap<u64, Arc<Mutex<rtrb::Producer<f32>>>>>,
    /// Shared SSRC → UserId map. Updated on every SpeakingStateUpdate so
    /// VoiceTick routing can look up which user an incoming SSRC belongs
    /// to. Shared with both VoiceReceiver clones.
    ssrc_map: Arc<Mutex<HashMap<u32, u64>>>,
    /// Optional speaker-hint client. `None` disables the feature entirely;
    /// Some(...) posts hints to a peer rpg_vox instance on every
    /// VoiceTick where a user has decoded audio.
    hint_client: Option<Arc<HintClient>>,
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

        let rec: Option<Arc<recorder::Recorder>> = if let Some(root) = &self.record_root {
            let session_root = root
                .join(self.guild_id.get().to_string())
                .join(self.channel_id.get().to_string());
            let session_dir = recorder::timestamped_session_dir(&session_root);
            match recorder::Recorder::new(session_dir.clone()) {
                Ok(r) => {
                    info!(dir = %session_dir.display(), "recording session started");
                    Some(Arc::new(r))
                }
                Err(err) => {
                    error!(?err, "failed to start recording session");
                    None
                }
            }
        } else {
            None
        };

        // Register receive-side event handlers whenever any downstream
        // consumer needs the events: recording, per-user audio outputs,
        // or the rpg_vox hint client (which needs SpeakingStateUpdate
        // for name resolution and VoiceTick for the per-frame speaker
        // set). Without any of these, the songbird DecodeMode is
        // DEFAULT (Decrypt only) and no decoded PCM would arrive anyway.
        let need_receiver = rec.is_some()
            || !self.audio_outputs.is_empty()
            || self.hint_client.is_some();
        if need_receiver {
            let mut driver = call.lock().await;
            driver.remove_all_global_events();
            let recorder_arc = rec.as_ref().map(Arc::clone);
            let hint_client_arc = self.hint_client.as_ref().map(Arc::clone);
            let make_receiver = || VoiceReceiver {
                recorder: recorder_arc.as_ref().map(Arc::clone),
                guild_id: self.guild_id,
                http: Arc::clone(&ctx.http),
                metadata: self.metadata.clone(),
                audio_outputs: Arc::clone(&self.audio_outputs),
                ssrc_map: Arc::clone(&self.ssrc_map),
                hint_client: hint_client_arc.as_ref().map(Arc::clone),
            };
            driver.add_global_event(CoreEvent::SpeakingStateUpdate.into(), make_receiver());
            driver.add_global_event(CoreEvent::VoiceTick.into(), make_receiver());
            drop(driver);
        }

        if let Some(rec) = rec {
            *self.active_recorder.lock().unwrap() = Some(rec);

            let notice = build_recording_notice(&self.operator);
            match self.channel_id.say(&ctx.http, &notice).await {
                Ok(msg) => {
                    *self.notice_message.lock().unwrap() = Some(msg.id);
                }
                Err(err) => {
                    warn!(
                        ?err,
                        "failed to post recording notice to text-in-voice \
                         (does the bot have SEND_MESSAGES on the channel?)"
                    );
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

        if let Some(md) = &self.metadata {
            if let Some(guild) = ctx.cache.guild(self.guild_id) {
                md.upsert_guild(self.guild_id.get(), &guild.name);
                if let Some(chan) = guild.channels.get(&self.channel_id) {
                    md.upsert_channel(self.channel_id.get(), self.guild_id.get(), &chan.name);
                }
            }
        }

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
        let active = self.active_recorder.lock().unwrap().take();
        if let Some(rec) = active {
            let started_at = rec.started_at();
            let ended_at = chrono::Local::now();
            let participants = rec.participants();
            rec.finalize();
            let session_dir = rec.session_dir();
            info!(dir = %session_dir.display(), "recording session finalized");
            drop(rec);

            let notice = build_recording_stop_notice(started_at, ended_at, &participants);
            if let Err(err) = self.channel_id.say(&ctx.http, &notice).await {
                warn!(?err, "failed to post recording-stopped notice");
            }

            let start_msg = self.notice_message.lock().unwrap().take();
            if let Some(msg_id) = start_msg {
                if let Err(err) = self.channel_id.delete_message(&ctx.http, msg_id).await {
                    warn!(?err, "failed to delete recording-start WARNING");
                }
            }

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
    /// None when audio outputs are configured but recording is off.
    recorder: Option<Arc<recorder::Recorder>>,
    guild_id: GuildId,
    http: Arc<Http>,
    metadata: Option<Arc<metadata::MetadataDb>>,
    audio_outputs: Arc<HashMap<u64, Arc<Mutex<rtrb::Producer<f32>>>>>,
    ssrc_map: Arc<Mutex<HashMap<u32, u64>>>,
    hint_client: Option<Arc<HintClient>>,
}

#[async_trait]
impl VoiceEventHandler for VoiceReceiver {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        match ctx {
            EventContext::SpeakingStateUpdate(speaking) => {
                let user_id = speaking.user_id.map(|u| u.0);
                if let Some(uid) = user_id {
                    self.ssrc_map.lock().unwrap().insert(speaking.ssrc, uid);
                }
                if let Some(rec) = &self.recorder {
                    rec.note_speaker(speaking.ssrc, user_id);
                }
                if let Some(uid) = user_id {
                    let http = Arc::clone(&self.http);
                    let recorder = self.recorder.as_ref().map(Arc::clone);
                    let guild_id = self.guild_id;
                    let metadata = self.metadata.clone();
                    let hint_client = self.hint_client.as_ref().map(Arc::clone);
                    tokio::spawn(async move {
                        if let Some(name) =
                            fetch_display_name(&http, guild_id, uid, metadata.as_deref()).await
                        {
                            if let Some(rec) = &recorder {
                                rec.note_display_name(uid, &name);
                            }
                            // Share the resolved name with the hint
                            // client so subsequent VoiceTick fires can
                            // include the user's readable label instead
                            // of just their user id.
                            if let Some(hc) = &hint_client {
                                hc.note_display_name(uid, &name);
                            }
                        }
                    });
                }
            }
            EventContext::VoiceTick(tick) => {
                let pcm: HashMap<u32, &[i16]> = tick
                    .speaking
                    .iter()
                    .filter_map(|(&ssrc, data)| data.decoded_voice.as_deref().map(|d| (ssrc, d)))
                    .collect();

                // Fire speaker hints for every user with decoded audio
                // this tick. The hint client throttles per-user so
                // ~20 ms VoiceTick cadence doesn't fan out into a POST
                // storm. Unknown SSRCs (no SpeakingStateUpdate yet) are
                // skipped; the next VoiceTick after the mapping
                // arrives will pick them up.
                if let Some(hc) = &self.hint_client {
                    if !pcm.is_empty() {
                        let ssrc_map = self.ssrc_map.lock().unwrap();
                        for &ssrc in pcm.keys() {
                            if let Some(&uid) = ssrc_map.get(&ssrc) {
                                hc.maybe_send(uid);
                            }
                        }
                    }
                }
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
                if let Some(rec) = &self.recorder {
                    rec.write_tick(&pcm, &opus_refs);
                }

                // Route decoded PCM to any per-user PipeWire output whose UID
                // matches. Best-effort: unknown SSRCs and unrouted users are
                // ignored; producer overrun (Discord ahead of consumer) drops
                // trailing samples for this tick.
                if !self.audio_outputs.is_empty() {
                    let ssrc_map = self.ssrc_map.lock().unwrap();
                    for (&ssrc, samples) in &pcm {
                        let Some(&uid) = ssrc_map.get(&ssrc) else { continue };
                        let Some(producer) = self.audio_outputs.get(&uid) else { continue };
                        let mut p = producer.lock().unwrap();
                        for &s in *samples {
                            // Discord decoded i16 → f32 in [-1.0, 1.0].
                            let f = (s as f32) / (i16::MAX as f32);
                            if p.push(f).is_err() {
                                break;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        None
    }
}

/// Resolve a Discord user's display name: guild nickname if set, otherwise
/// their global display name, otherwise their username. Sanitized for use in
/// a filename. Returns None on any failure or if nothing usable is left.
/// Additionally, if a metadata db is provided, upserts the raw identifiers
/// (username, global name, per-guild nick) as a side-effect — the DB stores
/// unsanitized values.
async fn fetch_display_name(
    http: &Http,
    guild_id: GuildId,
    user_id: u64,
    metadata: Option<&metadata::MetadataDb>,
) -> Option<String> {
    match http.get_member(guild_id, UserId::new(user_id)).await {
        Ok(member) => {
            let raw = member
                .nick
                .as_deref()
                .or(member.user.global_name.as_deref())
                .unwrap_or(&member.user.name);
            if let Some(md) = metadata {
                md.upsert_user(
                    user_id,
                    &member.user.name,
                    member.user.global_name.as_deref(),
                    raw,
                );
                md.upsert_guild_member(guild_id.get(), user_id, member.nick.as_deref());
            }
            (!raw.trim().is_empty()).then(|| raw.to_string())
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
                info!(
                    frames_5s = delta,
                    total = now,
                    overruns = over,
                    "sink activity"
                );
            }
        }
    });

    let recording = args.record.is_some();
    if recording {
        info!(dir = %args.record.as_ref().unwrap().display(), "voice recording enabled");
    }

    let metadata_path: Option<PathBuf> = args
        .metadata_db
        .clone()
        .or_else(|| args.record.as_ref().map(|r| r.join("metadata.db")));

    let metadata = if let Some(path) = &metadata_path {
        match metadata::MetadataDb::open(path) {
            Ok(db) => {
                info!(path = %path.display(), "metadata db opened");
                Some(Arc::new(db))
            }
            Err(err) => {
                error!(?err, path = %path.display(), "failed to open metadata db; continuing without it");
                None
            }
        }
    } else {
        None
    };

    // Per-user PipeWire audio outputs: one PW source node per configured
    // Discord uid. Ring buffers hold ~1s of 48 kHz stereo f32; overrun
    // (Discord ahead of downstream consumer) drops samples silently. Node
    // description picks up the display name from the metadata db if the
    // user has been observed in a prior session; otherwise falls back to
    // the raw uid until the next run.
    let output_uids = scan_audio_outputs();
    let mut audio_outputs: HashMap<u64, Arc<Mutex<rtrb::Producer<f32>>>> = HashMap::new();
    let mut output_handles: Vec<pw_output::Handle> = Vec::new();
    for (idx, uid) in output_uids.iter().enumerate() {
        let slot = idx + 1;
        let capacity = (48_000 * 2) as usize; // 1s stereo @ 48kHz
        let (producer, consumer) = RingBuffer::<f32>::new(capacity);
        let node_name = format!("discord-vox-out-{slot}");
        let label = metadata
            .as_ref()
            .and_then(|md| md.get_user_best_name(*uid).ok().flatten())
            .unwrap_or_else(|| uid.to_string());
        let node_description = format!("Discord Vox: {label}");
        match pw_output::spawn(
            pw_output::Config {
                node_name: node_name.clone(),
                node_description: node_description.clone(),
                sample_rate: 48_000,
                channels: 2,
            },
            consumer,
        ) {
            Ok(h) => {
                info!(slot, uid, node = %node_name, desc = %node_description, "audio output node started");
                audio_outputs.insert(*uid, Arc::new(Mutex::new(producer)));
                output_handles.push(h);
            }
            Err(err) => {
                error!(?err, slot, uid, "failed to start audio output node");
            }
        }
    }
    let audio_outputs = Arc::new(audio_outputs);

    let hint_client = HintClient::from_args(
        args.rpg_vox_url.as_deref(),
        args.rpg_vox_channel.as_deref(),
        args.rpg_vox_hint_interval_ms,
    );
    if let Some(hc) = &hint_client {
        info!(
            endpoint = %hc.endpoint,
            channel = ?hc.channel,
            interval_ms = args.rpg_vox_hint_interval_ms,
            "rpg_vox hint client configured",
        );
    }

    let handler = Handler {
        guild_id: GuildId::new(args.guild_id),
        channel_id: ChannelId::new(args.channel_id),
        consumer: Arc::new(Mutex::new(consumer)),
        sample_rate: args.sample_rate,
        channels: args.channels,
        record_root: args.record.clone(),
        operator: Operator {
            name: args.operator_name.clone(),
            discord: args.operator_discord.clone(),
            email: args.operator_email.clone(),
        },
        metadata: metadata.clone(),
        active_recorder: Mutex::new(None),
        notice_message: Mutex::new(None),
        audio_outputs: Arc::clone(&audio_outputs),
        ssrc_map: Arc::new(Mutex::new(HashMap::new())),
        hint_client,
        joined: AtomicBool::new(false),
        reconcile: AsyncMutex::new(()),
    };

    let intents = GatewayIntents::GUILDS | GatewayIntents::GUILD_VOICE_STATES;

    // Enable DecodeMode::Decode whenever any downstream consumer needs
    // decoded PCM: recording, per-user audio outputs, or the rpg_vox
    // hint client (which flags a user as "speaking now" based on
    // `decoded_voice.is_some()` per VoiceTick).
    let want_decode =
        recording || !audio_outputs.is_empty() || args.rpg_vox_url.is_some();
    let songbird_config = if want_decode {
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
    for h in output_handles.drain(..) {
        h.shutdown();
    }

    if let Err(err) = start_result {
        warn!(?err, "serenity client exited with error");
        return Err(err.into());
    }
    Ok(())
}
