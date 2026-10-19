//! Audio capture for vox_scribe: every backend delivers mono `f32`
//! chunks (~20 ms) on a channel, at whatever rate the device runs.
//!
//! * Linux: native PipeWire stream ([`pipewire`]), or ALSA through cpal
//!   ([`cpal_input`]) where PipeWire isn't running, as on PulseAudio
//!   systems: the mic only, no app capture or virtual sink.
//! * macOS: CoreAudio through cpal ([`cpal_input`]); one app's output
//!   through a Core Audio process tap ([`process_tap`], macOS 14.4+).
//! * Windows: WASAPI through cpal ([`cpal_input`]), which also captures
//!   what an output device is playing (loopback) when asked to record
//!   one, so system audio needs no virtual device.
//! * Any OS: decode a file ([`file`]).
//!
//! Also: playback to the default output ([`playback`]) and Ogg Opus
//! recordings ([`opus_file`]).
//!
//! The realtime callback only writes into an SPSC ring; a reader thread
//! mixes the device's channels down to mono — or picks out the single
//! channel [`OpenOptions::channel`] asks for — and forwards, so nothing
//! allocates on the audio thread.

// Elsewhere (iOS) there's no device backend; the iOS app takes only the
// file and Opus code and leaves the capture plumbing unused.
#![cfg_attr(
    not(any(
        target_os = "macos",
        target_os = "android",
        target_os = "linux",
        target_os = "windows"
    )),
    allow(dead_code, unused_imports)
)]

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Result;
use crossbeam_channel::{bounded, Receiver, Sender};

#[cfg(any(
    target_os = "macos",
    target_os = "android",
    target_os = "linux",
    target_os = "windows"
))]
pub mod cpal_input;
pub mod file;
pub mod opus_file;
#[cfg(target_os = "linux")]
pub mod pipewire;
pub mod playback;
#[cfg(target_os = "macos")]
pub mod process_tap;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod pw_dump;
pub mod resample;
pub mod stretch;

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// Value to pass as [`OpenOptions::device`].
    pub id: String,
    pub name: String,
    pub is_default: bool,
    /// An output device that records what it plays rather than an
    /// input: WASAPI loopback, on Windows only.
    pub loopback: bool,
    /// How many channels it captures, or 0 where the backend doesn't
    /// say (cpal, which would have to open each device to find out).
    /// More than one can be transcribed a channel at a time: see
    /// [`OpenOptions::channel`].
    pub channels: u16,
}

#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
    /// Device id or a case-insensitive substring of its name. `None`
    /// uses the system default input. On Windows, an output device
    /// records what it is playing, and `"loopback"` names the default
    /// output without having to look its name up.
    pub device: Option<String>,
    /// One channel of a multichannel device on its own (0-based),
    /// instead of all of them mixed down. Two mics on one stereo
    /// interface are two sources, one speaker each, this way. Parsed
    /// from a `-d` value by [`split_channel`].
    pub channel: Option<usize>,
    /// PipeWire only: register a virtual `Audio/Sink` node that other
    /// apps can play into, instead of capturing from a source.
    pub virtual_sink: bool,
    /// Capture what one app is playing instead of an input: a PID, or a
    /// case-insensitive substring of the app's name (see [`Backend::list_apps`]).
    pub app: Option<String>,
}

impl OpenOptions {
    /// One `-d` value: the device, and the channel of it when the value
    /// names one (`rode:FL`).
    pub fn for_device(spec: &str) -> Self {
        let (device, channel) = split_channel(spec);
        Self {
            device: Some(device.to_string()),
            channel,
            ..Default::default()
        }
    }

    /// `name` with the channel this captures named after it, so two
    /// channels of one device don't both show up under its name alone.
    pub fn label(&self, name: &str) -> String {
        match self.channel {
            Some(c) => format!("{name}:{}", channel_label(c)),
            None => name.to_string(),
        }
    }
}

/// Splits a `-d` value into the device and, where it names one, the
/// 0-based channel of it to transcribe: `rode:FL`, `rode:right`,
/// `rode:ch3`.
///
/// Only those spellings count, so an ALSA device id that happens to end
/// in a colon and a number (`hw:1`) still names the whole device — which
/// is why a channel by number is written `ch3` rather than `3`.
pub fn split_channel(spec: &str) -> (&str, Option<usize>) {
    let Some((device, want)) = spec.rsplit_once(':') else {
        return (spec, None);
    };
    if device.is_empty() {
        return (spec, None);
    }
    let channel = match want.to_lowercase().as_str() {
        "l" | "fl" | "left" => Some(0),
        "r" | "fr" | "right" => Some(1),
        other => other
            .strip_prefix("ch")
            .and_then(|n| n.parse::<usize>().ok())
            // `ch0` is no channel: they are 1-based written this way.
            .and_then(|n| n.checked_sub(1)),
    };
    match channel {
        Some(c) => (device, Some(c)),
        None => (spec, None),
    }
}

/// How `-d` spells a channel, and so how a source captured from one is
/// named: `FL`, `FR`, `ch3`, ….
pub fn channel_label(channel: usize) -> String {
    match channel {
        0 => "FL".into(),
        1 => "FR".into(),
        n => format!("ch{}", n + 1),
    }
}

/// How a device's channels become the one mono stream scribe
/// transcribes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mix {
    /// Every channel averaged: the whole device, however many capsules.
    #[default]
    Down,
    /// Channel `n` (0-based) alone, as [`OpenOptions::channel`] asks.
    Channel(usize),
}

impl Mix {
    /// What [`OpenOptions::channel`] asks of a device that turned out to
    /// have `channels` of them. Asking for one it hasn't got is an
    /// error: silently transcribing a different mic is worse than
    /// refusing to start.
    pub(crate) fn of(channel: Option<usize>, channels: usize) -> Result<Self> {
        match channel {
            None => Ok(Mix::Down),
            Some(n) if n < channels => Ok(Mix::Channel(n)),
            Some(n) => anyhow::bail!(
                "this device has {channels} channel(s), so there is no channel {} to transcribe",
                n + 1
            ),
        }
    }

    /// One frame of interleaved samples as one mono sample.
    fn apply(self, frame: &[f32]) -> f32 {
        match self {
            Mix::Down => frame.iter().sum::<f32>() / frame.len().max(1) as f32,
            // A channel that isn't there is silence rather than a
            // panic; `Mix::of` rejects that before the stream opens.
            Mix::Channel(n) => frame.get(n).copied().unwrap_or(0.0),
        }
    }
}

/// An app that is (or was) playing audio, a candidate for
/// [`OpenOptions::app`].
#[derive(Debug, Clone)]
pub struct AppInfo {
    pub pid: u32,
    /// Process name.
    pub name: String,
    /// Bundle id on macOS, `application.name` on PipeWire.
    pub id: String,
    /// Producing sound right now.
    pub playing: bool,
    /// Core Audio process object on macOS, stream node id on PipeWire.
    pub(crate) object: u32,
}

impl AppInfo {
    /// `want` is a PID or a case-insensitive substring of name or id.
    pub fn matches(&self, want: &str) -> bool {
        if let Ok(pid) = want.parse::<u32>() {
            return self.pid == pid;
        }
        let want = want.to_lowercase();
        self.name.to_lowercase().contains(&want) || self.id.to_lowercase().contains(&want)
    }
}

pub trait Backend {
    fn name(&self) -> &'static str;
    fn list_devices(&self) -> Result<Vec<DeviceInfo>>;
    /// Apps whose output can be captured with [`OpenOptions::app`].
    fn list_apps(&self) -> Result<Vec<AppInfo>> {
        anyhow::bail!(
            "per-app capture is not supported by the {} backend",
            self.name()
        )
    }
    fn open(&self, opts: &OpenOptions) -> Result<Capture>;
}

/// The native backend for this OS.
pub fn default_backend() -> Result<Box<dyn Backend>> {
    #[cfg(any(target_os = "macos", target_os = "android"))]
    {
        Ok(Box::new(cpal_input::Cpal))
    }
    #[cfg(target_os = "linux")]
    {
        if pipewire::running() {
            Ok(Box::new(pipewire::PipeWire))
        } else {
            Ok(Box::new(cpal_input::Cpal))
        }
    }
    #[cfg(target_os = "windows")]
    {
        Ok(Box::new(cpal_input::Cpal))
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "android",
        target_os = "windows"
    )))]
    {
        anyhow::bail!("no live audio backend for this OS; use a file input")
    }
}

/// A running capture. Dropping it stops the stream.
pub struct Capture {
    pub sample_rate: u32,
    /// Human-readable name of what is being captured.
    pub device: String,
    /// Samples dropped because the reader fell behind the device.
    pub overruns: Arc<AtomicU64>,
    chunks: Receiver<Vec<f32>>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Capture {
    pub fn chunks(&self) -> &Receiver<Vec<f32>> {
        &self.chunks
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// Shared plumbing for backends: an SPSC ring for the RT side and a
/// reader thread that mixes `channels` down to mono (or picks one of
/// them, per [`Mix`]) and forwards ~20 ms chunks.
pub(crate) struct Plumbing {
    pub producer: rtrb::Producer<f32>,
    pub overruns: Arc<AtomicU64>,
    pub stop: Arc<AtomicBool>,
    chunks: Receiver<Vec<f32>>,
    reader: JoinHandle<()>,
}

impl Plumbing {
    pub fn new(sample_rate: u32, channels: usize, mix: Mix) -> Self {
        Self::with_input_rate(
            sample_rate,
            channels,
            mix,
            Arc::new(AtomicU32::new(sample_rate)),
        )
    }

    /// Like [`Plumbing::new`] for a device whose rate can change under
    /// it: samples arrive at `input_rate` (updated by the backend) and
    /// are resampled to the fixed `sample_rate` the session started with.
    pub fn with_input_rate(
        sample_rate: u32,
        channels: usize,
        mix: Mix,
        input_rate: Arc<AtomicU32>,
    ) -> Self {
        let channels = channels.max(1);
        // Two seconds of headroom.
        let (producer, consumer) =
            rtrb::RingBuffer::<f32>::new(sample_rate as usize * channels * 2);
        let (tx, rx) = bounded::<Vec<f32>>(512);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_r = stop.clone();
        let chunk = (sample_rate as usize / 50).max(1);
        let reader = std::thread::Builder::new()
            .name("vox-audio-reader".into())
            .spawn(move || {
                reader_loop(
                    consumer,
                    Reading {
                        channels,
                        mix,
                        chunk,
                        input_rate,
                        out_rate: sample_rate,
                    },
                    tx,
                    stop_r,
                )
            })
            .expect("spawn audio reader");
        Self {
            producer,
            overruns: Arc::new(AtomicU64::new(0)),
            stop,
            chunks: rx,
            reader,
        }
    }

    /// Split into the RT-side producer and a constructor for the
    /// [`Capture`] once the backend thread is running.
    pub fn into_parts(self) -> (rtrb::Producer<f32>, CaptureParts) {
        (
            self.producer,
            CaptureParts {
                overruns: self.overruns,
                stop: self.stop,
                chunks: self.chunks,
                reader: self.reader,
            },
        )
    }
}

pub(crate) struct CaptureParts {
    pub overruns: Arc<AtomicU64>,
    pub stop: Arc<AtomicBool>,
    chunks: Receiver<Vec<f32>>,
    reader: JoinHandle<()>,
}

impl CaptureParts {
    pub fn finish(
        self,
        sample_rate: u32,
        device: String,
        backend_thread: JoinHandle<()>,
    ) -> Capture {
        Capture {
            sample_rate,
            device,
            overruns: self.overruns,
            chunks: self.chunks,
            stop: self.stop,
            threads: vec![backend_thread, self.reader],
        }
    }
}

/// Push interleaved samples from an RT callback, counting drops.
pub(crate) fn push_rt(
    producer: &mut rtrb::Producer<f32>,
    overruns: &AtomicU64,
    samples: impl Iterator<Item = f32>,
) {
    let mut dropped = 0u64;
    for s in samples {
        if producer.push(s).is_err() {
            dropped += 1;
        }
    }
    if dropped > 0 {
        overruns.fetch_add(dropped, Ordering::Relaxed);
    }
}

/// What the reader thread makes of what the device gives it.
struct Reading {
    /// Samples per frame in the ring, interleaved.
    channels: usize,
    /// Those channels as the one mono sample scribe transcribes.
    mix: Mix,
    /// Samples per chunk sent on (~20 ms).
    chunk: usize,
    /// The rate the device is running at now, which it may change
    /// under us.
    input_rate: Arc<AtomicU32>,
    /// The fixed rate the session started at.
    out_rate: u32,
}

fn reader_loop(
    mut consumer: rtrb::Consumer<f32>,
    r: Reading,
    tx: Sender<Vec<f32>>,
    stop: Arc<AtomicBool>,
) {
    let Reading {
        channels,
        mix,
        chunk,
        input_rate,
        out_rate,
    } = r;
    let mut out: Vec<f32> = Vec::with_capacity(chunk);
    let mut frame: Vec<f32> = Vec::with_capacity(channels);
    let mut mono: Vec<f32> = Vec::new();
    let mut rate = out_rate;
    let mut resampler = resample::Linear::new(rate, out_rate);
    while !stop.load(Ordering::Relaxed) {
        let available = consumer.slots();
        if available == 0 {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        let Ok(read) = consumer.read_chunk(available) else {
            continue;
        };
        for s in read {
            frame.push(s);
            if frame.len() == channels {
                mono.push(mix.apply(&frame));
                frame.clear();
            }
        }
        let now = input_rate.load(Ordering::Relaxed);
        if now != rate && now > 0 {
            rate = now;
            resampler = resample::Linear::new(rate, out_rate);
        }
        resampler.process(&mono, &mut out);
        mono.clear();
        while out.len() >= chunk {
            let rest = out.split_off(chunk);
            if tx.send(std::mem::replace(&mut out, rest)).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed `secs` of samples at `rate` into a plumbing whose session runs
    /// at 48 kHz; return how many session samples come out.
    fn through(rate: u32, secs: f32, input_rate: &Arc<AtomicU32>, p: &mut rtrb::Producer<f32>) {
        input_rate.store(rate, Ordering::Relaxed);
        let n = (rate as f32 * secs) as usize;
        for i in 0..n {
            while p.push((i as f32 * 0.01).sin()).is_err() {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        // Let the reader see this batch at this rate.
        std::thread::sleep(Duration::from_millis(100));
    }

    #[test]
    fn a_channel_suffix_names_one_channel() {
        assert_eq!(split_channel("rode:FL"), ("rode", Some(0)));
        assert_eq!(split_channel("rode:fr"), ("rode", Some(1)));
        assert_eq!(split_channel("rode:Left"), ("rode", Some(0)));
        assert_eq!(split_channel("rode:RIGHT"), ("rode", Some(1)));
        assert_eq!(split_channel("rode:ch1"), ("rode", Some(0)));
        assert_eq!(split_channel("rode:ch6"), ("rode", Some(5)));
        // Only the last colon, so a node name may hold one of its own.
        assert_eq!(split_channel("a:b:ch2"), ("a:b", Some(1)));
    }

    #[test]
    fn anything_else_after_a_colon_is_part_of_the_device() {
        // An ALSA id, not channel 1 of `hw`: numbers need `ch`.
        assert_eq!(split_channel("hw:1"), ("hw:1", None));
        assert_eq!(
            split_channel("hw:CARD=PCH,DEV=0"),
            ("hw:CARD=PCH,DEV=0", None)
        );
        assert_eq!(split_channel("rode:ch0"), ("rode:ch0", None));
        assert_eq!(split_channel("rode:centre"), ("rode:centre", None));
        assert_eq!(split_channel("rode"), ("rode", None));
        assert_eq!(split_channel(":FL"), (":FL", None));
    }

    #[test]
    fn a_device_value_carries_its_channel() {
        let o = OpenOptions::for_device("rode:FR");
        assert_eq!(o.device.as_deref(), Some("rode"));
        assert_eq!(o.channel, Some(1));
        let o = OpenOptions::for_device("hw:1");
        assert_eq!(o.device.as_deref(), Some("hw:1"));
        assert_eq!(o.channel, None);
    }

    #[test]
    fn mixing_down_averages_and_a_channel_is_taken_as_it_is() {
        assert_eq!(Mix::Down.apply(&[0.5, -0.5]), 0.0);
        assert_eq!(Mix::Down.apply(&[1.0, 0.0]), 0.5);
        assert_eq!(Mix::Channel(0).apply(&[1.0, 0.0]), 1.0);
        assert_eq!(Mix::Channel(1).apply(&[1.0, 0.25]), 0.25);
    }

    #[test]
    fn a_channel_the_device_hasnt_got_is_refused() {
        assert_eq!(Mix::of(None, 2).unwrap(), Mix::Down);
        assert_eq!(Mix::of(Some(1), 2).unwrap(), Mix::Channel(1));
        let err = Mix::of(Some(2), 2).unwrap_err().to_string();
        assert!(err.contains("2 channel(s)"), "{err}");
        assert!(err.contains("channel 3"), "{err}");
    }

    /// Two channels in, the asked-for one out.
    #[test]
    fn the_reader_picks_one_channel_out_of_the_interleaved_frames() {
        let (mut producer, parts) = Plumbing::new(48_000, 2, Mix::Channel(1)).into_parts();
        // Left is silence, right a steady 0.5: 0.5 is what comes out.
        for _ in 0..48_000 {
            while producer.push(0.0).is_err() {
                std::thread::sleep(Duration::from_millis(1));
            }
            while producer.push(0.5).is_err() {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        std::thread::sleep(Duration::from_millis(300));
        parts.stop.store(true, Ordering::SeqCst);
        let out: Vec<f32> = parts.chunks.try_iter().flatten().collect();
        assert!(!out.is_empty(), "nothing came through");
        assert!(
            out.iter().all(|&s| (s - 0.5).abs() < 1e-6),
            "the other channel leaked in"
        );
    }

    #[test]
    fn reader_resamples_when_the_device_rate_changes() {
        let rate = Arc::new(AtomicU32::new(48_000));
        let (mut producer, parts) =
            Plumbing::with_input_rate(48_000, 1, Mix::Down, rate.clone()).into_parts();
        through(48_000, 0.5, &rate, &mut producer);
        through(16_000, 0.5, &rate, &mut producer);
        through(44_100, 0.5, &rate, &mut producer);
        std::thread::sleep(Duration::from_millis(200));
        parts.stop.store(true, Ordering::SeqCst);
        let got: usize = parts.chunks.try_iter().map(|c| c.len()).sum();
        // 1.5 s at the session rate, give or take the last partial chunk.
        assert!((got as i64 - 72_000).abs() <= 1_000, "{got}");
    }
}
