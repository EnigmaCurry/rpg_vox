//! Native PipeWire source node — one per configured Discord audio output.
//!
//! Registers as `Audio/Source` in-process, on a dedicated OS thread running
//! the PipeWire main loop. The realtime `process` callback drains an SPSC
//! `Consumer<f32>` (fed from the tokio side when the routed user speaks)
//! and writes interleaved f32 samples into the negotiated output buffer.
//! Emits silence on underrun so the node keeps producing a valid stream
//! whether the user is speaking or not.

use anyhow::{Context as _, Result};
use rtrb::Consumer;
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
    /// Total frames (samples/channel) written to PipeWire since startup.
    #[allow(dead_code)]
    pub frames_out: Arc<AtomicU64>,
    /// Times the RT callback ran with nothing in the consumer and had to
    /// pad silence. Steady-state should be near-constant when the routed
    /// user is silent; only interesting for drop detection while talking.
    #[allow(dead_code)]
    pub underruns: Arc<AtomicU64>,
}

impl Handle {
    pub fn shutdown(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub fn spawn(cfg: Config, consumer: Consumer<f32>) -> Result<Handle> {
    let stop = Arc::new(AtomicBool::new(false));
    let frames_out = Arc::new(AtomicU64::new(0));
    let underruns = Arc::new(AtomicU64::new(0));

    let stop_thread = stop.clone();
    let frames_thread = frames_out.clone();
    let underruns_thread = underruns.clone();
    let thread = thread::Builder::new()
        .name(format!("pw-out-{}", cfg.node_name))
        .spawn(move || {
            if let Err(err) = run(cfg, consumer, stop_thread, frames_thread, underruns_thread) {
                error!(?err, "PipeWire output thread exited with error");
            }
        })
        .context("failed to spawn PipeWire output thread")?;

    Ok(Handle {
        stop,
        thread: Some(thread),
        frames_out,
        underruns,
    })
}

fn run(
    cfg: Config,
    consumer: Consumer<f32>,
    stop: Arc<AtomicBool>,
    frames_out: Arc<AtomicU64>,
    underruns: Arc<AtomicU64>,
) -> Result<()> {
    pw::init();

    let mainloop = MainLoop::new(None).context("MainLoop::new")?;
    let context = Context::new(&mainloop).context("Context::new")?;
    let core = context.connect(None).context("Context::connect")?;

    let props = properties! {
        *keys::MEDIA_TYPE => "Audio",
        *keys::MEDIA_CATEGORY => "Playback",
        *keys::MEDIA_ROLE => "Communication",
        *keys::MEDIA_CLASS => "Audio/Source",
        *keys::NODE_NAME => cfg.node_name.as_str(),
        *keys::NODE_DESCRIPTION => cfg.node_description.as_str(),
        "node.virtual" => "true",
    };

    let stream = Stream::new(&core, &cfg.node_name, props).context("Stream::new")?;

    struct StreamState {
        consumer: Consumer<f32>,
        channels: u32,
        frames_out: Arc<AtomicU64>,
        underruns: Arc<AtomicU64>,
    }
    let state = StreamState {
        consumer,
        channels: cfg.channels,
        frames_out: frames_out.clone(),
        underruns: underruns.clone(),
    };

    let _listener = stream
        .add_local_listener_with_user_data::<StreamState>(state)
        .state_changed(|_, _, old, new| {
            info!(?old, ?new, "PipeWire output stream state changed");
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
            let sample_bytes = std::mem::size_of::<f32>();
            let frame_bytes = sample_bytes * state.channels as usize;

            let n_samples;
            {
                let Some(bytes) = data.data() else { return };
                let out: &mut [f32] = bytemuck_cast_slice_mut(bytes);
                n_samples = out.len();
                let mut written = 0usize;
                while written < n_samples {
                    match state.consumer.pop() {
                        Ok(s) => {
                            out[written] = s;
                            written += 1;
                        }
                        Err(_) => break,
                    }
                }
                if written < n_samples {
                    for s in &mut out[written..] {
                        *s = 0.0;
                    }
                    let prev = state.underruns.fetch_add(1, Ordering::Relaxed);
                    if prev == 0 || prev.is_power_of_two() {
                        debug!(underruns = prev + 1, "audio output ring underrun");
                    }
                }
            }
            state
                .frames_out
                .fetch_add((n_samples / state.channels.max(1) as usize) as u64, Ordering::Relaxed);

            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = frame_bytes as _;
            *chunk.size_mut() = (n_samples * sample_bytes) as _;
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
            libspa::utils::Direction::Output,
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
        "PipeWire output main loop entering run()"
    );
    mainloop.run();
    info!(node = %cfg.node_name, "PipeWire output main loop exited");
    drop(timer);
    if !stop.load(Ordering::SeqCst) {
        warn!(node = %cfg.node_name, "PipeWire output main loop exited without shutdown request");
    }
    Ok(())
}

fn bytemuck_cast_slice_mut(bytes: &mut [u8]) -> &mut [f32] {
    let len = bytes.len() / std::mem::size_of::<f32>();
    let ptr = bytes.as_mut_ptr() as *mut f32;
    unsafe { std::slice::from_raw_parts_mut(ptr, len) }
}
