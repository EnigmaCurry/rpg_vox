//! Native PipeWire capture.
//!
//! Opens a mono `F32LE` stream on a dedicated main-loop thread; PipeWire's
//! adapter handles downmix and format conversion. By default the stream
//! is a capture client that autoconnects to the default source (or the
//! node named by `--device`) and can be relinked in qpwgraph/Helvum.
//! With `virtual_sink` it instead registers an `Audio/Sink` node that
//! other apps can play into, like rpg_vox's `-vox` sinks.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use crossbeam_channel::bounded;
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
use tracing::{error, info};

use crate::{push_rt, Backend, Capture, DeviceInfo, OpenOptions, Plumbing};

/// Rate we ask PipeWire for; it resamples whatever the graph runs at.
const RATE: u32 = 48_000;
const NODE_NAME: &str = "vox_scribe";

pub struct PipeWire;

impl Backend for PipeWire {
    fn name(&self) -> &'static str {
        "pipewire"
    }

    /// Lists sources and sinks via `pw-dump`. Pass a sink's name to
    /// capture its monitor (what's playing on it).
    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        let out = std::process::Command::new("pw-dump")
            .output()
            .context("run pw-dump (is pipewire-utils installed?)")?;
        let objects: serde_json::Value =
            serde_json::from_slice(&out.stdout).context("parse pw-dump")?;
        let mut devices = Vec::new();
        for obj in objects.as_array().into_iter().flatten() {
            let props = &obj["info"]["props"];
            let class = props["media.class"].as_str().unwrap_or_default();
            let kind = match class {
                "Audio/Source" | "Audio/Source/Virtual" => "source",
                "Audio/Sink" => "sink monitor",
                _ => continue,
            };
            let Some(name) = props["node.name"].as_str() else {
                continue;
            };
            let desc = props["node.description"].as_str().unwrap_or(name);
            devices.push(DeviceInfo {
                id: name.to_string(),
                name: format!("{desc} ({kind})"),
                is_default: false,
            });
        }
        Ok(devices)
    }

    fn open(&self, opts: &OpenOptions) -> Result<Capture> {
        let plumbing = Plumbing::new(RATE, 1);
        let (producer, parts) = plumbing.into_parts();
        let stop = parts.stop.clone();
        let overruns = parts.overruns.clone();
        let opts_t = opts.clone();
        let (ready_tx, ready_rx) = bounded::<Result<()>>(1);
        let thread = std::thread::Builder::new()
            .name("vox-pipewire".into())
            .spawn(move || {
                if let Err(err) = run(opts_t, producer, stop, overruns, &ready_tx) {
                    error!(?err, "PipeWire capture thread exited with error");
                    let _ = ready_tx.send(Err(err));
                }
            })
            .context("spawn PipeWire thread")?;
        ready_rx.recv().context("PipeWire thread died")??;
        let label = match (&opts.device, opts.virtual_sink) {
            (_, true) => format!("PipeWire sink '{NODE_NAME}'"),
            (Some(d), false) => format!("PipeWire {d}"),
            (None, false) => "PipeWire default source".to_string(),
        };
        Ok(parts.finish(RATE, label, thread))
    }
}

fn run(
    opts: OpenOptions,
    producer: rtrb::Producer<f32>,
    stop: Arc<AtomicBool>,
    overruns: Arc<AtomicU64>,
    ready: &crossbeam_channel::Sender<Result<()>>,
) -> Result<()> {
    pw::init();
    let mainloop = MainLoop::new(None).context("MainLoop::new")?;
    let context = Context::new(&mainloop).context("Context::new")?;
    let core = context.connect(None).context("Context::connect")?;

    let mut props = properties! {
        *keys::MEDIA_TYPE => "Audio",
        *keys::MEDIA_CATEGORY => "Capture",
        *keys::MEDIA_ROLE => "Production",
        *keys::NODE_NAME => NODE_NAME,
        *keys::NODE_DESCRIPTION => "vox_scribe transcription",
    };
    if opts.virtual_sink {
        props.insert(*keys::MEDIA_CLASS, "Audio/Sink");
        props.insert("node.virtual", "true");
    } else if let Some(target) = &opts.device {
        props.insert("target.object", target.as_str());
        // A sink name means "capture what's playing on it".
        props.insert(
            "stream.capture.sink",
            if is_sink(target) { "true" } else { "false" },
        );
    }

    let stream = Stream::new(&core, NODE_NAME, props).context("Stream::new")?;

    struct State {
        producer: rtrb::Producer<f32>,
        overruns: Arc<AtomicU64>,
    }
    let _listener = stream
        .add_local_listener_with_user_data(State { producer, overruns })
        .state_changed(|_, _, old, new| info!(?old, ?new, "PipeWire capture state"))
        .process(|stream, state| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            let Some(data) = datas.first_mut() else {
                return;
            };
            let (offset, size) = {
                let chunk = data.chunk();
                (chunk.offset() as usize, chunk.size() as usize)
            };
            let Some(bytes) = data.data() else { return };
            let end = (offset + size).min(bytes.len());
            let valid = &bytes[offset.min(end)..end];
            let samples = valid
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]));
            push_rt(&mut state.producer, &state.overruns, samples);
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
            libspa::utils::Direction::Input,
            None,
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
            &mut params,
        )
        .context("Stream::connect")?;
    let _ = ready.send(Ok(()));

    let mainloop_weak = mainloop.downgrade();
    let timer = mainloop.loop_().add_timer(move |_| {
        if stop.load(Ordering::SeqCst) {
            if let Some(ml) = mainloop_weak.upgrade() {
                ml.quit();
            }
        }
    });
    timer.update_timer(
        Some(std::time::Duration::from_millis(100)),
        Some(std::time::Duration::from_millis(100)),
    );
    mainloop.run();
    drop(timer);
    Ok(())
}

/// Whether `node` names an `Audio/Sink` (best effort via `pw-dump`).
fn is_sink(node: &str) -> bool {
    PipeWire
        .list_devices()
        .map(|ds| {
            ds.iter()
                .any(|d| d.id == node && d.name.ends_with("(sink monitor)"))
        })
        .unwrap_or(false)
}
