//! Per-session voice recorder.
//!
//! For each auto-join session we open a fresh directory containing:
//!   * `mixed.wav` — everyone summed to a single stereo track, transcoded to
//!     lossless FLAC at session end (see [`transcode_session`]).
//!   * `user-<uid>[-<name>].opus` for each speaker — the raw Opus packets
//!     Discord sent us, wrapped in an Ogg container. No re-encoding, so
//!     these are lossless relative to what left the speaker's client.
//!     Silence is skipped: per-user files are speech-only and are not time
//!     aligned with `mixed.wav`.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context as _, Result};
use hound::{SampleFormat, WavSpec, WavWriter};
use ogg::writing::{PacketWriteEndInfo, PacketWriter};
use tracing::{debug, error, info, warn};

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;
/// Discord voice ticks are 20 ms. 48 kHz × 0.02 s = 960 frames per tick.
pub const FRAMES_PER_TICK: usize = 960;
/// Interleaved stereo samples per tick.
pub const SAMPLES_PER_TICK: usize = FRAMES_PER_TICK * (CHANNELS as usize);

const WAV_SPEC: WavSpec = WavSpec {
    channels: CHANNELS,
    sample_rate: SAMPLE_RATE,
    bits_per_sample: 16,
    sample_format: SampleFormat::Int,
};

type MixedWriter = WavWriter<BufWriter<File>>;
type UserWriter = PacketWriter<'static, BufWriter<File>>;

struct State {
    session_dir: PathBuf,
    mixed: Option<MixedWriter>,
    per_user: HashMap<u32, PerUser>,
    ssrc_to_user: HashMap<u32, u64>,
    /// Resolved, sanitized display name per user id (filled in async after
    /// note_speaker fires; may never arrive if the fetch fails).
    user_display_name: HashMap<u64, String>,
}

struct PerUser {
    writer: UserWriter,
    /// Current on-disk path of this file — tracked so we can rename it in
    /// place if the SSRC → UserId mapping arrives after the writer is opened.
    path: PathBuf,
    /// Ogg stream serial. Using SSRC directly is unique-enough per session.
    serial: u32,
    /// Cumulative 48 kHz samples emitted for this stream (Opus granule
    /// position). Advances by 960 per 20 ms packet regardless of the
    /// packet's channel count.
    granulepos: u64,
    /// Most recently written packet, held back so `finalize()` can flag it
    /// with `EndStream` instead of `NormalPacket`. Store the granulepos at
    /// its end so we can pass it through unchanged.
    pending: Option<(Vec<u8>, u64)>,
}

pub struct Recorder {
    state: Mutex<State>,
}

impl Recorder {
    pub fn new(session_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&session_dir)
            .with_context(|| format!("create session dir {}", session_dir.display()))?;
        let mixed_path = session_dir.join("mixed.wav");
        let mixed = WavWriter::create(&mixed_path, WAV_SPEC)
            .with_context(|| format!("open {}", mixed_path.display()))?;
        Ok(Self {
            state: Mutex::new(State {
                session_dir,
                mixed: Some(mixed),
                per_user: HashMap::new(),
                ssrc_to_user: HashMap::new(),
                user_display_name: HashMap::new(),
            }),
        })
    }

    pub fn session_dir(&self) -> PathBuf {
        self.state.lock().unwrap().session_dir.clone()
    }

    /// Remember the SSRC → Discord user id mapping. Discord may deliver
    /// audio for a new SSRC before the SpeakingStateUpdate that carries the
    /// user id, so if we've already opened a `ssrc-<num>.opus` fallback file
    /// we rename it in place. The underlying fd keeps writing to the same
    /// inode across the rename.
    pub fn note_speaker(&self, ssrc: u32, user_id: Option<u64>) {
        let Some(uid) = user_id else { return };
        let mut state = self.state.lock().unwrap();
        state.ssrc_to_user.insert(ssrc, uid);
        state.refresh_path(ssrc);
    }

    /// Attach a human-readable display name (already sanitized) to a user
    /// id, so any writers for that user get renamed to include it. Empty
    /// names are ignored.
    pub fn note_display_name(&self, user_id: u64, sanitized: &str) {
        if sanitized.is_empty() {
            return;
        }
        let mut state = self.state.lock().unwrap();
        state
            .user_display_name
            .insert(user_id, sanitized.to_string());
        let ssrcs: Vec<u32> = state
            .ssrc_to_user
            .iter()
            .filter_map(|(&ssrc, &uid)| (uid == user_id).then_some(ssrc))
            .collect();
        for ssrc in ssrcs {
            state.refresh_path(ssrc);
        }
    }

    /// Process a single 20 ms voice tick.
    ///
    /// `speaking_pcm`: SSRC → decoded stereo i16 PCM (used only for the
    /// mixed track). `speaking_opus`: SSRC → raw Opus packet payload
    /// (written verbatim to each speaker's Ogg file). The two hashmaps are
    /// keyed identically in normal operation, but each side is tolerant of
    /// missing entries.
    pub fn write_tick(
        &self,
        speaking_pcm: &HashMap<u32, &[i16]>,
        speaking_opus: &HashMap<u32, &[u8]>,
    ) {
        let mut state = self.state.lock().unwrap();

        // Mixed track: sum in i32 to avoid wraparound, hard-clip to i16.
        let mut mix = [0i32; SAMPLES_PER_TICK];
        for samples in speaking_pcm.values() {
            for (i, &s) in samples.iter().take(SAMPLES_PER_TICK).enumerate() {
                mix[i] = mix[i].saturating_add(s as i32);
            }
        }
        if let Some(mixed) = state.mixed.as_mut() {
            for &s in &mix {
                let clipped = s.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                if let Err(err) = mixed.write_sample(clipped) {
                    error!(?err, "mixed writer failed");
                    state.mixed = None;
                    break;
                }
            }
        }

        // Per-user Opus files: append the raw packet, if any.
        for (&ssrc, payload) in speaking_opus {
            if payload.is_empty() {
                continue;
            }
            let channels = opus_toc_channels(payload[0]);
            state.write_opus_packet(ssrc, payload, channels);
        }
    }

    /// Flush and close every writer, writing a final EndStream page on each
    /// per-user file so it terminates cleanly. Idempotent.
    pub fn finalize(&self) {
        let mut state = self.state.lock().unwrap();
        if let Some(w) = state.mixed.take() {
            if let Err(err) = w.finalize() {
                warn!(?err, "mixed writer finalize failed");
            }
        }
        let per_user = std::mem::take(&mut state.per_user);
        for (ssrc, mut pu) in per_user {
            if let Some((payload, gp)) = pu.pending.take() {
                if let Err(err) = pu.writer.write_packet(
                    payload,
                    pu.serial,
                    PacketWriteEndInfo::EndStream,
                    gp,
                ) {
                    warn!(?err, ssrc, "per-user EOS write failed");
                }
            }
            // Dropping `pu.writer` drops the BufWriter, which flushes.
        }
        debug!(dir = %state.session_dir.display(), "recorder finalized");
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.finalize();
    }
}

impl State {
    fn desired_filename(&self, ssrc: u32) -> String {
        match self.ssrc_to_user.get(&ssrc) {
            Some(uid) => match self.user_display_name.get(uid) {
                Some(name) => format!("user-{uid}-{name}.opus"),
                None => format!("user-{uid}.opus"),
            },
            None => format!("ssrc-{ssrc}.opus"),
        }
    }

    fn refresh_path(&mut self, ssrc: u32) {
        let desired = self.session_dir.join(self.desired_filename(ssrc));
        let Some(pu) = self.per_user.get_mut(&ssrc) else {
            return;
        };
        if pu.path == desired {
            return;
        }
        match fs::rename(&pu.path, &desired) {
            Ok(()) => {
                debug!(
                    ssrc,
                    from = %pu.path.display(),
                    to = %desired.display(),
                    "renamed per-user recording"
                );
                pu.path = desired;
            }
            Err(err) => {
                warn!(?err, ssrc, "failed to rename per-user recording");
            }
        }
    }

    /// Open a per-user Ogg-Opus writer on first packet, writing the
    /// OpusHead + OpusTags headers so the file is a valid Ogg stream from
    /// byte zero.
    fn writer_for(&mut self, ssrc: u32, channels: u8) -> Option<&mut PerUser> {
        if !self.per_user.contains_key(&ssrc) {
            let path = self.session_dir.join(self.desired_filename(ssrc));
            let file = match File::create(&path) {
                Ok(f) => f,
                Err(err) => {
                    error!(?err, path = %path.display(), "failed to open per-user .opus");
                    return None;
                }
            };
            let mut writer = PacketWriter::new(BufWriter::new(file));
            let serial = ssrc;
            let head = build_opus_head(channels);
            let tags = build_opus_tags();
            if let Err(err) = writer.write_packet(head, serial, PacketWriteEndInfo::EndPage, 0) {
                error!(?err, ssrc, "OpusHead write failed");
                return None;
            }
            if let Err(err) = writer.write_packet(tags, serial, PacketWriteEndInfo::EndPage, 0) {
                error!(?err, ssrc, "OpusTags write failed");
                return None;
            }
            self.per_user.insert(
                ssrc,
                PerUser {
                    writer,
                    path,
                    serial,
                    granulepos: 0,
                    pending: None,
                },
            );
        }
        self.per_user.get_mut(&ssrc)
    }

    fn write_opus_packet(&mut self, ssrc: u32, payload: &[u8], channels: u8) {
        let Some(pu) = self.writer_for(ssrc, channels) else {
            return;
        };
        pu.granulepos = pu.granulepos.saturating_add(FRAMES_PER_TICK as u64);
        // Flush the previously-buffered packet as a normal one, then buffer
        // this one so finalize() can flag it with EndStream.
        if let Some((prev, prev_gp)) = pu.pending.take() {
            if let Err(err) = pu.writer.write_packet(
                prev,
                pu.serial,
                PacketWriteEndInfo::NormalPacket,
                prev_gp,
            ) {
                warn!(?err, ssrc, "opus packet write failed");
            }
        }
        pu.pending = Some((payload.to_vec(), pu.granulepos));
    }
}

/// Decode the "stereo" bit out of an Opus TOC byte. Layout: `CCCCCSFF`
/// (5 bits config, 1 bit stereo, 2 bits frame-count code). We only need
/// the S bit to set the correct channel count in the OpusHead header.
fn opus_toc_channels(toc: u8) -> u8 {
    if (toc >> 2) & 1 == 1 { 2 } else { 1 }
}

/// Build the OpusHead identification header packet per RFC 7845 §5.1.
fn build_opus_head(channels: u8) -> Vec<u8> {
    let mut buf = Vec::with_capacity(19);
    buf.extend_from_slice(b"OpusHead");
    buf.push(1); // version
    buf.push(channels);
    buf.extend_from_slice(&312u16.to_le_bytes()); // pre_skip (samples @ 48 kHz)
    buf.extend_from_slice(&SAMPLE_RATE.to_le_bytes()); // original input rate
    buf.extend_from_slice(&0i16.to_le_bytes()); // output gain (Q7.8, 0 = no change)
    buf.push(0); // channel mapping family 0 (mono/stereo, RTP-style)
    buf
}

/// Build the OpusTags comment header packet per RFC 7845 §5.2. Minimal —
/// vendor string only, zero user comments.
fn build_opus_tags() -> Vec<u8> {
    let vendor = b"discord_vox";
    let mut buf = Vec::with_capacity(8 + 4 + vendor.len() + 4);
    buf.extend_from_slice(b"OpusTags");
    buf.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    buf.extend_from_slice(vendor);
    buf.extend_from_slice(&0u32.to_le_bytes()); // 0 user comments
    buf
}

pub fn timestamped_session_dir(root: &Path) -> PathBuf {
    let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    root.join(stamp)
}

/// Turn a Discord display name into something safe to embed in a filename.
/// Spaces become underscores; path/shell metacharacters and control chars
/// become dashes; the result is trimmed of leading/trailing junk and capped
/// at 60 characters. Returns "" if nothing usable is left.
pub fn sanitize_display_name(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| match c {
            ' ' | '\t' | '\n' | '\r' => '_',
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '-',
            c if c.is_control() => '-',
            c => c,
        })
        .collect();
    let trim: &[char] = &['-', '_', '.'];
    let trimmed = out.trim_matches(trim);
    if trimmed.chars().count() > 60 {
        out = trimmed.chars().take(60).collect::<String>();
        out = out.trim_matches(trim).to_string();
    } else {
        out = trimmed.to_string();
    }
    out
}

/// Post-process a finalized session directory: transcode `mixed.wav` to
/// lossless FLAC, deleting the source on success. Per-user files are
/// already Ogg-Opus so they don't need touching. Spawned from
/// `Handler::leave` so it runs off the hot path.
///
/// A short sleep at the start lets any in-flight display-name renames land
/// before we snapshot the directory contents.
pub async fn transcode_session(session_dir: PathBuf) {
    use tokio::process::Command;

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let mixed_wav = session_dir.join("mixed.wav");
    if !mixed_wav.exists() {
        return;
    }
    let mixed_flac = session_dir.join("mixed.flac");

    let status = Command::new("ffmpeg")
        .arg("-loglevel").arg("error")
        .arg("-y")
        .arg("-i").arg(&mixed_wav)
        .arg("-c:a").arg("flac")
        .arg(&mixed_flac)
        .status()
        .await;

    match status {
        Ok(s) if s.success() => {
            if let Err(err) = std::fs::remove_file(&mixed_wav) {
                warn!(?err, path = %mixed_wav.display(), "transcode ok but rm failed");
            } else {
                info!(out = %mixed_flac.display(), "transcoded mixed track");
            }
        }
        Ok(s) => {
            warn!(?s, "ffmpeg exited non-zero; leaving mixed.wav in place");
        }
        Err(err) => {
            warn!(
                ?err,
                "failed to spawn ffmpeg; leaving mixed.wav in place (is ffmpeg on PATH?)"
            );
        }
    }
}
