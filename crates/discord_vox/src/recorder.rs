//! Per-session voice recorder.
//!
//! For each auto-join session we open a fresh directory containing:
//!   * `user-<uid>.wav` for each speaker (silence-padded so all files line up
//!     with the mixed file on the same timeline).
//!   * `mixed.wav` — everyone summed to a single stereo track.
//!
//! Files are 48 kHz, 16-bit signed, stereo (Discord's native decoded format).

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context as _, Result};
use hound::{SampleFormat, WavSpec, WavWriter};
use tracing::{debug, error, warn};

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

type Writer = WavWriter<BufWriter<File>>;

struct State {
    session_dir: PathBuf,
    mixed: Option<Writer>,
    /// Per-SSRC writer + interleaved-sample count already written, so we can
    /// silence-pad late arrivals up to the current session timeline.
    per_user: HashMap<u32, PerUser>,
    ssrc_to_user: HashMap<u32, u64>,
    /// Resolved, sanitized display name per user id (filled in async after
    /// note_speaker fires; may never arrive if the fetch fails).
    user_display_name: HashMap<u64, String>,
    /// Total 20 ms ticks observed since the recorder started.
    tick_counter: u64,
}

struct PerUser {
    writer: Writer,
    /// Current on-disk path of this file — tracked so we can rename it in
    /// place if the SSRC → UserId mapping arrives after the writer is opened.
    path: PathBuf,
    /// Total samples (interleaved L,R,L,R,...) written to this file.
    samples_written: u64,
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
                tick_counter: 0,
            }),
        })
    }

    pub fn session_dir(&self) -> PathBuf {
        self.state.lock().unwrap().session_dir.clone()
    }

    /// Remember the SSRC → Discord user id mapping. Discord may deliver
    /// audio for a new SSRC before the SpeakingStateUpdate that carries the
    /// user id, so if we've already opened a `ssrc-<num>.wav` fallback file
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
    /// `speaking`: SSRC → decoded stereo i16 PCM (may be shorter than
    /// SAMPLES_PER_TICK on packet loss).
    /// `silent`: SSRCs known to be in the channel but not speaking this tick;
    /// we silence-pad their files so they stay time-aligned.
    pub fn write_tick(&self, speaking: &HashMap<u32, &[i16]>, silent: &[u32]) {
        let mut state = self.state.lock().unwrap();
        state.tick_counter = state.tick_counter.saturating_add(1);

        let mut mix = [0i32; SAMPLES_PER_TICK];

        for (&ssrc, samples) in speaking {
            for (i, &s) in samples.iter().take(SAMPLES_PER_TICK).enumerate() {
                mix[i] = mix[i].saturating_add(s as i32);
            }
            state.write_user(ssrc, samples);
        }

        for &ssrc in silent {
            state.write_silence(ssrc, SAMPLES_PER_TICK);
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
    }

    /// Flush and close every writer. Idempotent.
    pub fn finalize(&self) {
        let mut state = self.state.lock().unwrap();
        if let Some(w) = state.mixed.take() {
            if let Err(err) = w.finalize() {
                warn!(?err, "mixed writer finalize failed");
            }
        }
        let per_user = std::mem::take(&mut state.per_user);
        for (ssrc, pu) in per_user {
            if let Err(err) = pu.writer.finalize() {
                warn!(?err, ssrc, "per-user writer finalize failed");
            }
        }
        debug!(
            dir = %state.session_dir.display(),
            ticks = state.tick_counter,
            "recorder finalized"
        );
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
                Some(name) => format!("user-{uid}-{name}.wav"),
                None => format!("user-{uid}.wav"),
            },
            None => format!("ssrc-{ssrc}.wav"),
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

    fn writer_for(&mut self, ssrc: u32) -> Option<&mut PerUser> {
        if !self.per_user.contains_key(&ssrc) {
            let name = self.desired_filename(ssrc);
            let path = self.session_dir.join(&name);
            let writer = match WavWriter::create(&path, WAV_SPEC) {
                Ok(w) => w,
                Err(err) => {
                    error!(?err, path = %path.display(), "failed to open per-user wav");
                    return None;
                }
            };
            let mut pu = PerUser {
                writer,
                path: path.clone(),
                samples_written: 0,
            };
            // Silence-pad this file up to the current session position so it
            // stays aligned with the mixed track.
            let pad_ticks = self.tick_counter.saturating_sub(1);
            let pad_samples = pad_ticks * SAMPLES_PER_TICK as u64;
            for _ in 0..pad_samples {
                if pu.writer.write_sample(0i16).is_err() {
                    break;
                }
            }
            pu.samples_written = pad_samples;
            self.per_user.insert(ssrc, pu);
        }
        self.per_user.get_mut(&ssrc)
    }

    fn write_user(&mut self, ssrc: u32, samples: &[i16]) {
        let Some(pu) = self.writer_for(ssrc) else {
            return;
        };
        let take = samples.len().min(SAMPLES_PER_TICK);
        for &s in &samples[..take] {
            if pu.writer.write_sample(s).is_err() {
                return;
            }
        }
        // If the tick was short (packet loss), pad out the rest with silence
        // so this file stays aligned tick-for-tick with everything else.
        for _ in take..SAMPLES_PER_TICK {
            if pu.writer.write_sample(0i16).is_err() {
                return;
            }
        }
        pu.samples_written += SAMPLES_PER_TICK as u64;
    }

    fn write_silence(&mut self, ssrc: u32, samples: usize) {
        // Only pad if we've already opened a writer for this speaker; there's
        // no point opening a file for someone who's never spoken.
        let Some(pu) = self.per_user.get_mut(&ssrc) else {
            return;
        };
        for _ in 0..samples {
            if pu.writer.write_sample(0i16).is_err() {
                return;
            }
        }
        pu.samples_written += samples as u64;
    }
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
