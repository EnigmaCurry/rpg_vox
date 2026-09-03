//! Native PipeWire source node.
//!
//! Runs the PipeWire main loop on a dedicated OS thread. The realtime `process`
//! callback drains f32 mono samples from an `rtrb::Consumer` and writes them into
//! the negotiated output buffer. When the ring runs dry, the callback emits
//! silence so downstream consumers (Firefox → Discord) see a continuous stream.

use anyhow::{Context as _, Result};
use rtrb::Consumer;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
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
    pod::{serialize::PodSerializer, Object, Pod, Value},
};

pub struct Config {
    pub node_name: String,
    pub node_description: String,
    pub sample_rate: u32,
}

pub struct Handle {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
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
    let stop_thread = stop.clone();

    let thread = thread::Builder::new()
        .name("pw-source".into())
        .spawn(move || {
            if let Err(err) = run(cfg, consumer, stop_thread) {
                error!(?err, "PipeWire source thread exited with error");
            }
        })
        .context("failed to spawn PipeWire source thread")?;

    Ok(Handle {
        stop,
        thread: Some(thread),
    })
}

/// Per-stream state accessible from the realtime `process` callback.
struct StreamState {
    consumer: Consumer<f32>,
    underruns: u64,
}

fn run(cfg: Config, consumer: Consumer<f32>, stop: Arc<AtomicBool>) -> Result<()> {
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

    let state = StreamState {
        consumer,
        underruns: 0,
    };

    let _listener = stream
        .add_local_listener_with_user_data::<StreamState>(state)
        .state_changed(|_, _, old, new| {
            info!(?old, ?new, "PipeWire stream state changed");
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
            let stride = std::mem::size_of::<f32>();

            let (frames_written, requested_frames) = if let Some(slice) = data.data() {
                let requested = slice.len() / stride;
                let mut written = 0usize;
                // SAFETY: we treat the byte slice as f32 samples for writing. Alignment
                // is guaranteed by PipeWire buffer allocation for F32LE format.
                let out: &mut [f32] = bytemuck_cast_slice_mut(slice);
                while written < out.len() {
                    match state.consumer.pop() {
                        Ok(sample) => {
                            out[written] = sample;
                            written += 1;
                        }
                        Err(_) => break,
                    }
                }
                // Fill any remainder with silence.
                if written < out.len() {
                    for s in &mut out[written..] {
                        *s = 0.0;
                    }
                    if written == 0 {
                        state.underruns = state.underruns.saturating_add(1);
                        if state.underruns.is_power_of_two() {
                            debug!(underruns = state.underruns, "audio ring underrun");
                        }
                    }
                }
                (out.len(), requested)
            } else {
                (0, 0)
            };
            let _ = requested_frames;

            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = stride as _;
            *chunk.size_mut() = (stride * frames_written) as _;
        })
        .register()
        .context("Stream::register")?;

    // Build the audio format param.
    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(cfg.sample_rate);
    info.set_channels(1);

    let obj = Object {
        type_: libspa::sys::SPA_TYPE_OBJECT_Format,
        id: libspa::sys::SPA_PARAM_EnumFormat,
        properties: info.into(),
    };
    let values: Vec<u8> = PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj))
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

    // Poll the stop flag from the main loop with a periodic timer so we can
    // exit cleanly without needing to signal into pipewire from another thread.
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

    info!("PipeWire main loop entering run()");
    mainloop.run();
    info!("PipeWire main loop exited");
    drop(timer);
    if !stop.load(Ordering::SeqCst) {
        warn!("PipeWire main loop exited without shutdown request");
    }
    Ok(())
}

/// Reinterpret a byte slice as an f32 slice without pulling in a whole crate.
/// Buffers handed to us by PipeWire for F32LE format are guaranteed to be
/// aligned and sized for f32.
fn bytemuck_cast_slice_mut(bytes: &mut [u8]) -> &mut [f32] {
    let len = bytes.len() / std::mem::size_of::<f32>();
    let ptr = bytes.as_mut_ptr() as *mut f32;
    // SAFETY: caller has verified length divisibility; PipeWire provides
    // properly aligned buffers for the negotiated F32LE sample format.
    unsafe { std::slice::from_raw_parts_mut(ptr, len) }
}
