//! Native PipeWire sink node.
//!
//! Registers an `Audio/Sink` in-process, on a dedicated OS thread running the
//! PipeWire main loop. Any app whose output is routed into this sink (via
//! Helvum, qpwgraph, or `pw-link`) hands us 48 kHz stereo f32 samples in the
//! realtime `process` callback, which are pushed into a caller-supplied SPSC
//! ring buffer. The consumer end feeds songbird.

use anyhow::{Context as _, Result};
use rtrb::{Producer, PushError};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::thread::{self, JoinHandle};
use tracing::{debug, error, info, warn};

use pipewire as pw;
use pw::{
    context::Context,
    keys,
    main_loop::MainLoop,
    properties::properties,
    stream::{Stream, StreamFlags},
};

use libspa::{
    param::audio::{AudioFormat, AudioInfoRaw},
    pod::{Object, Pod, Value, serialize::PodSerializer},
};

pub struct Config {
    pub node_name: String,
    pub node_description: String,
    pub sample_rate: u32,
    pub channels: u32,
}

pub struct Handle {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// Total interleaved frames observed by the realtime callback since
    /// startup (one "frame" = one sample per channel).
    pub frames_in: Arc<AtomicU64>,
    /// Times the RT callback dropped samples because the consumer wasn't
    /// draining fast enough. If this climbs, Discord is behind PipeWire.
    pub overruns: Arc<AtomicU64>,
}

impl Handle {
    pub fn shutdown(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub fn spawn(cfg: Config, producer: Producer<f32>) -> Result<Handle> {
    let stop = Arc::new(AtomicBool::new(false));
    let frames_in = Arc::new(AtomicU64::new(0));
    let overruns = Arc::new(AtomicU64::new(0));

    let stop_thread = stop.clone();
    let frames_thread = frames_in.clone();
    let overruns_thread = overruns.clone();
    let thread = thread::Builder::new()
        .name("pw-sink".into())
        .spawn(move || {
            if let Err(err) = run(cfg, producer, stop_thread, frames_thread, overruns_thread) {
                error!(?err, "PipeWire sink thread exited with error");
            }
        })
        .context("failed to spawn PipeWire sink thread")?;

    Ok(Handle {
        stop,
        thread: Some(thread),
        frames_in,
        overruns,
    })
}

fn run(
    cfg: Config,
    producer: Producer<f32>,
    stop: Arc<AtomicBool>,
    frames_in: Arc<AtomicU64>,
    overruns: Arc<AtomicU64>,
) -> Result<()> {
    pw::init();

    let mainloop = MainLoop::new(None).context("MainLoop::new")?;
    let context = Context::new(&mainloop).context("Context::new")?;
    let core = context.connect(None).context("Context::connect")?;

    let props = properties! {
        *keys::MEDIA_TYPE => "Audio",
        *keys::MEDIA_CATEGORY => "Capture",
        *keys::MEDIA_ROLE => "Communication",
        *keys::MEDIA_CLASS => "Audio/Sink",
        *keys::NODE_NAME => cfg.node_name.as_str(),
        *keys::NODE_DESCRIPTION => cfg.node_description.as_str(),
        "node.virtual" => "true",
    };

    let stream = Stream::new(&core, &cfg.node_name, props).context("Stream::new")?;

    struct StreamState {
        producer: Producer<f32>,
        channels: u32,
        frames_in: Arc<AtomicU64>,
        overruns: Arc<AtomicU64>,
    }
    let state = StreamState {
        producer,
        channels: cfg.channels,
        frames_in: frames_in.clone(),
        overruns: overruns.clone(),
    };

    let _listener = stream
        .add_local_listener_with_user_data::<StreamState>(state)
        .state_changed(|_, _, old, new| {
            info!(?old, ?new, "PipeWire sink stream state changed");
        })
        .process(|stream, state| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let data = &mut datas[0];

            let (offset, size) = {
                let chunk = data.chunk();
                (chunk.offset() as usize, chunk.size() as usize)
            };

            let Some(bytes) = data.data() else { return };
            let end = (offset + size).min(bytes.len());
            let valid = &bytes[offset..end];
            let samples: &[f32] = bytemuck_cast_slice(valid);

            let mut dropped = 0u64;
            for &s in samples {
                match state.producer.push(s) {
                    Ok(()) => {}
                    Err(PushError::Full(_)) => {
                        dropped = dropped.saturating_add(1);
                    }
                }
            }
            if dropped > 0 {
                let prev = state.overruns.fetch_add(dropped, Ordering::Relaxed);
                if prev == 0 || (prev + dropped).is_power_of_two() {
                    debug!(dropped, total = prev + dropped, "sink ring overrun");
                }
            }

            let frames = samples.len() / state.channels.max(1) as usize;
            state.frames_in.fetch_add(frames as u64, Ordering::Relaxed);
        })
        .register()
        .context("Stream::register")?;

    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(cfg.sample_rate);
    info.set_channels(cfg.channels);
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
            libspa::utils::Direction::Input,
            None,
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
            &mut params,
        )
        .context("Stream::connect")?;

    let stop_check = stop.clone();
    let mainloop_weak = mainloop.downgrade();
    let timer = mainloop.loop_().add_timer(move |_| {
        if stop_check.load(Ordering::SeqCst) {
            if let Some(ml) = mainloop_weak.upgrade() {
                ml.quit();
            }
        }
    });
    timer.update_timer(
        Some(std::time::Duration::from_millis(200)),
        Some(std::time::Duration::from_millis(200)),
    );

    info!(
        node = %cfg.node_name,
        rate = cfg.sample_rate,
        channels = cfg.channels,
        "PipeWire sink main loop entering run()"
    );
    mainloop.run();
    info!("PipeWire sink main loop exited");
    drop(timer);
    if !stop.load(Ordering::SeqCst) {
        warn!("PipeWire sink main loop exited without shutdown request");
    }
    Ok(())
}

/// Reinterpret a byte slice as an f32 slice. PipeWire aligns buffers for the
/// negotiated F32LE sample format.
fn bytemuck_cast_slice(bytes: &[u8]) -> &[f32] {
    let len = bytes.len() / std::mem::size_of::<f32>();
    let ptr = bytes.as_ptr() as *const f32;
    unsafe { std::slice::from_raw_parts(ptr, len) }
}
