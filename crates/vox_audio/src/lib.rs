//! Audio capture for vox_scribe: every backend delivers mono `f32`
//! chunks (~20 ms) on a channel, at whatever rate the device runs.
//!
//! * Linux: native PipeWire stream ([`pipewire`]).
//! * macOS: CoreAudio through cpal ([`coreaudio`]); one app's output
//!   through a Core Audio process tap ([`process_tap`], macOS 14.4+).
//! * Any OS: decode a file ([`file`]).
//!
//! Also: playback to the default output ([`playback`]) and Ogg Opus
//! recordings ([`opus_file`]).
//!
//! The realtime callback only writes into an SPSC ring; a reader thread
//! downmixes and forwards, so nothing allocates on the audio thread.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Result;
use crossbeam_channel::{bounded, Receiver, Sender};

#[cfg(target_os = "macos")]
pub mod coreaudio;
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

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// Value to pass as [`OpenOptions::device`].
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
    /// Device id or a case-insensitive substring of its name. `None`
    /// uses the system default input.
    pub device: Option<String>,
    /// PipeWire only: register a virtual `Audio/Sink` node that other
    /// apps can play into, instead of capturing from a source.
    pub virtual_sink: bool,
    /// Capture what one app is playing instead of an input: a PID, or a
    /// case-insensitive substring of the app's name (see [`Backend::list_apps`]).
    pub app: Option<String>,
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
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(coreaudio::CoreAudio))
    }
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(pipewire::PipeWire))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
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
/// reader thread that downmixes to mono and forwards ~20 ms chunks.
pub(crate) struct Plumbing {
    pub producer: rtrb::Producer<f32>,
    pub overruns: Arc<AtomicU64>,
    pub stop: Arc<AtomicBool>,
    chunks: Receiver<Vec<f32>>,
    reader: JoinHandle<()>,
}

impl Plumbing {
    pub fn new(sample_rate: u32, channels: usize) -> Self {
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
            .spawn(move || reader_loop(consumer, channels, chunk, tx, stop_r))
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

fn reader_loop(
    mut consumer: rtrb::Consumer<f32>,
    channels: usize,
    chunk: usize,
    tx: Sender<Vec<f32>>,
    stop: Arc<AtomicBool>,
) {
    let mut out: Vec<f32> = Vec::with_capacity(chunk);
    let mut frame: Vec<f32> = Vec::with_capacity(channels);
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
                out.push(frame.iter().sum::<f32>() / channels as f32);
                frame.clear();
                if out.len() >= chunk
                    && tx
                        .send(std::mem::replace(&mut out, Vec::with_capacity(chunk)))
                        .is_err()
                {
                    return;
                }
            }
        }
    }
}
