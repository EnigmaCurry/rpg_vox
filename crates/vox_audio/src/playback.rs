//! Mono `f32` playback to the default output: CoreAudio through cpal on
//! macOS, a native PipeWire stream on Linux.
//!
//! The caller pushes samples into an SPSC ring; the device callback
//! pulls from it and counts what it actually played, which is the clock
//! the caller shows. Pausing and flushing (for seeks) are flags the
//! callback obeys, so the realtime side never blocks or allocates.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};

/// Ring size: how far ahead of the speaker the caller can run.
const BUFFER: Duration = Duration::from_millis(200);

#[derive(Default)]
pub(crate) struct Shared {
    /// Frames played since the last flush.
    pub played: AtomicU64,
    pub paused: AtomicBool,
    /// Set by [`Output::flush`]; the callback empties the ring, zeroes
    /// `played` and clears it.
    pub flush: AtomicBool,
    pub stop: AtomicBool,
}

impl Shared {
    /// Fill `frames` frames of `channels` interleaved samples via `put`.
    pub fn fill(
        &self,
        ring: &mut rtrb::Consumer<f32>,
        frames: usize,
        mut put: impl FnMut(usize, f32),
    ) {
        if self.flush.load(Ordering::Acquire) {
            let n = ring.slots();
            if let Ok(chunk) = ring.read_chunk(n) {
                chunk.commit_all();
            }
            self.played.store(0, Ordering::Release);
            self.flush.store(false, Ordering::Release);
        }
        let paused = self.paused.load(Ordering::Relaxed);
        let mut played = 0;
        for f in 0..frames {
            let s = if paused {
                0.0
            } else {
                match ring.pop() {
                    Ok(s) => {
                        played += 1;
                        s
                    }
                    Err(_) => 0.0,
                }
            };
            put(f, s);
        }
        if played > 0 {
            self.played.fetch_add(played, Ordering::Release);
        }
    }
}

/// An open output stream. Dropping it stops playback.
pub struct Output {
    /// Rate [`Output::push`] expects.
    pub sample_rate: u32,
    pub device: String,
    ring: rtrb::Producer<f32>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Output {
    /// Open the default output device.
    pub fn open() -> Result<Self> {
        let shared = Arc::new(Shared::default());
        let (rate, device, ring, thread) = open_backend(shared.clone())?;
        Ok(Self {
            sample_rate: rate,
            device,
            ring,
            shared,
            thread: Some(thread),
        })
    }

    /// Room in the ring, in samples.
    pub fn free(&self) -> usize {
        self.ring.slots()
    }

    /// Queue as much of `samples` as fits; returns how many were taken.
    pub fn push(&mut self, samples: &[f32]) -> usize {
        let n = samples.len().min(self.ring.slots());
        if let Ok(mut chunk) = self.ring.write_chunk_uninit(n) {
            let (a, b) = chunk.as_mut_slices();
            for (dst, src) in a.iter_mut().chain(b.iter_mut()).zip(samples) {
                dst.write(*src);
            }
            // SAFETY: all `n` slots were written above.
            unsafe { chunk.commit_all() };
        }
        n
    }

    /// Frames played since the last [`Output::flush`].
    pub fn played(&self) -> u64 {
        self.shared.played.load(Ordering::Acquire)
    }

    /// Samples queued but not yet played.
    pub fn queued(&self) -> usize {
        self.ring.buffer().capacity() - self.ring.slots()
    }

    pub fn set_paused(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::Relaxed);
    }

    /// Drop everything queued and restart the played counter. Waits
    /// (briefly) for the device callback to do it.
    pub fn flush(&mut self) {
        self.shared.flush.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_millis(500);
        while self.shared.flush.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

type Opened = (u32, String, rtrb::Producer<f32>, JoinHandle<()>);

fn ring_for(rate: u32) -> (rtrb::Producer<f32>, rtrb::Consumer<f32>) {
    rtrb::RingBuffer::new((rate as u128 * BUFFER.as_millis() / 1000) as usize)
}

#[cfg(any(target_os = "macos", target_os = "android"))]
fn open_backend(shared: Arc<Shared>) -> Result<Opened> {
    use anyhow::{anyhow, bail};
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{FromSample, SampleFormat, SizedSample};

    fn build<T: SizedSample + FromSample<f32>>(
        device: &cpal::Device,
        cfg: cpal::StreamConfig,
        mut ring: rtrb::Consumer<f32>,
        shared: Arc<Shared>,
    ) -> Result<cpal::Stream> {
        let channels = cfg.channels as usize;
        Ok(device.build_output_stream(
            cfg,
            move |out: &mut [T], _: &_| {
                let frames = out.len() / channels.max(1);
                shared.fill(&mut ring, frames, |f, s| {
                    let v = T::from_sample(s);
                    for c in &mut out[f * channels..(f + 1) * channels] {
                        *c = v;
                    }
                });
            },
            |e: cpal::Error| tracing::warn!("audio output error: {e}"),
            None,
        )?)
    }

    let (ready_tx, ready_rx) =
        crossbeam_channel::bounded::<Result<(u32, String, rtrb::Producer<f32>)>>(1);
    // cpal::Stream is !Send on macOS, so it lives on its own thread.
    let thread = std::thread::Builder::new()
        .name("vox-playback".into())
        .spawn(move || {
            let started = (|| -> Result<_> {
                let host = cpal::default_host();
                let device = host
                    .default_output_device()
                    .ok_or_else(|| anyhow!("no output device"))?;
                let name = device
                    .description()
                    .map(|d| d.name().to_string())
                    .unwrap_or_else(|_| "default output".into());
                let config = device
                    .default_output_config()
                    .context("default output config")?;
                let rate = config.sample_rate();
                let format = config.sample_format();
                let (producer, consumer) = ring_for(rate);
                let cfg: cpal::StreamConfig = config.into();
                let sh = shared.clone();
                let stream = match format {
                    SampleFormat::F32 => build::<f32>(&device, cfg, consumer, sh)?,
                    SampleFormat::I16 => build::<i16>(&device, cfg, consumer, sh)?,
                    SampleFormat::I32 => build::<i32>(&device, cfg, consumer, sh)?,
                    other => bail!("unsupported output sample format {other}"),
                };
                stream.play().context("start output stream")?;
                Ok((stream, rate, name, producer))
            })();
            match started {
                Ok((stream, rate, name, producer)) => {
                    let _ = ready_tx.send(Ok((rate, name, producer)));
                    while !shared.stop.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    drop(stream);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        })
        .context("spawn playback thread")?;
    let (rate, name, producer) = ready_rx.recv().context("playback thread died")??;
    Ok((rate, name, producer, thread))
}

#[cfg(target_os = "linux")]
fn open_backend(shared: Arc<Shared>) -> Result<Opened> {
    use libspa::param::audio::{AudioFormat, AudioInfoRaw};
    use libspa::pod::{serialize::PodSerializer, Object, Pod, Value};
    use pipewire as pw;
    use pw::{
        context::Context,
        keys,
        main_loop::MainLoop,
        properties::properties,
        stream::{Stream, StreamFlags},
    };

    /// PipeWire converts to whatever the graph runs at.
    const RATE: u32 = 48_000;
    let (producer, consumer) = ring_for(RATE);
    let (ready_tx, ready_rx) = crossbeam_channel::bounded::<Result<()>>(1);
    let thread = std::thread::Builder::new()
        .name("vox-playback".into())
        .spawn(move || {
            let run = || -> Result<()> {
                pw::init();
                let mainloop = MainLoop::new(None).context("MainLoop::new")?;
                let context = Context::new(&mainloop).context("Context::new")?;
                let core = context.connect(None).context("Context::connect")?;
                let props = properties! {
                    *keys::MEDIA_TYPE => "Audio",
                    *keys::MEDIA_CATEGORY => "Playback",
                    *keys::MEDIA_ROLE => "Production",
                    *keys::NODE_NAME => "vox_scribe_play",
                    *keys::NODE_DESCRIPTION => "vox_scribe playback",
                };
                let stream = Stream::new(&core, "vox_scribe_play", props).context("Stream::new")?;
                struct State {
                    ring: rtrb::Consumer<f32>,
                    shared: Arc<Shared>,
                }
                let _listener = stream
                    .add_local_listener_with_user_data(State {
                        ring: consumer,
                        shared: shared.clone(),
                    })
                    .process(|stream, state| {
                        let Some(mut buffer) = stream.dequeue_buffer() else {
                            return;
                        };
                        let datas = buffer.datas_mut();
                        let Some(data) = datas.first_mut() else {
                            return;
                        };
                        let frames = match data.data() {
                            Some(bytes) => {
                                let frames = bytes.len() / 4;
                                state.shared.fill(&mut state.ring, frames, |f, s| {
                                    bytes[f * 4..f * 4 + 4].copy_from_slice(&s.to_le_bytes());
                                });
                                frames
                            }
                            None => 0,
                        };
                        let chunk = data.chunk_mut();
                        *chunk.offset_mut() = 0;
                        *chunk.stride_mut() = 4;
                        *chunk.size_mut() = (frames * 4) as u32;
                    })
                    .register()
                    .context("Stream::register")?;

                let mut info = AudioInfoRaw::new();
                info.set_format(AudioFormat::F32LE);
                info.set_rate(RATE);
                info.set_channels(1);
                let obj = Object {
                    type_: libspa::sys::SPA_TYPE_OBJECT_Format,
                    id: libspa::sys::SPA_PARAM_EnumFormat,
                    properties: info.into(),
                };
                let values: Vec<u8> =
                    PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj))
                        .context("serialize format pod")?
                        .0
                        .into_inner();
                let mut params = [Pod::from_bytes(&values).context("format pod parse")?];
                stream
                    .connect(
                        libspa::utils::Direction::Output,
                        None,
                        StreamFlags::AUTOCONNECT
                            | StreamFlags::MAP_BUFFERS
                            | StreamFlags::RT_PROCESS,
                        &mut params,
                    )
                    .context("Stream::connect")?;
                let _ = ready_tx.send(Ok(()));

                let mainloop_weak = mainloop.downgrade();
                let stop = shared.clone();
                let timer = mainloop.loop_().add_timer(move |_| {
                    if stop.stop.load(Ordering::SeqCst) {
                        if let Some(ml) = mainloop_weak.upgrade() {
                            ml.quit();
                        }
                    }
                });
                timer.update_timer(
                    Some(Duration::from_millis(100)),
                    Some(Duration::from_millis(100)),
                );
                mainloop.run();
                drop(timer);
                Ok(())
            };
            if let Err(e) = run() {
                tracing::error!(?e, "PipeWire playback thread exited with error");
                let _ = ready_tx.send(Err(e));
            }
        })
        .context("spawn playback thread")?;
    ready_rx.recv().context("playback thread died")??;
    Ok((RATE, "PipeWire default output".into(), producer, thread))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
fn open_backend(_shared: Arc<Shared>) -> Result<Opened> {
    anyhow::bail!("no audio output backend for this OS")
}
