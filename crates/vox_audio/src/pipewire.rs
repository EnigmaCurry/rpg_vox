//! Native PipeWire capture.
//!
//! Opens an `F32LE` stream on a dedicated main-loop thread, in the
//! channel layout the source itself runs in, and mixes it down to mono
//! in the reader thread. Matching the source's layout is what gets
//! every channel linked: the session manager links ports by channel
//! position, so a lone `MONO` port on a stereo source matches `FL` and
//! leaves `FR` — the other half of the room — unlinked and unheard.
//! [`OpenOptions::channel`] keeps one channel instead of mixing, which
//! is how two mics on one stereo receiver become two speakers.
//!
//! By default the stream
//! is a capture client that autoconnects to the default source (or the
//! node named by `--device`) and can be relinked in qpwgraph/Helvum.
//! With `virtual_sink` it instead registers an `Audio/Sink` node that
//! other apps can play into, like rpg_vox's `-vox` sinks. With `app`
//! the stream is left unconnected and a helper thread links every output
//! stream of the matching app(s) to it with `pw-link`, rescanning so new
//! streams (a browser's next tab, a restarted player) are picked up. The
//! app keeps playing to its usual sink.

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

use crate::pw_dump::Layout;
use crate::{push_rt, pw_dump, AppInfo, Backend, Capture, DeviceInfo, Mix, OpenOptions, Plumbing};

/// Rate we ask PipeWire for; it resamples whatever the graph runs at.
const RATE: u32 = 48_000;
const NODE_NAME: &str = "vox_scribe";
/// How often app mode looks for new streams to link.
const RELINK: std::time::Duration = std::time::Duration::from_secs(2);

/// Our node's name: unique in app mode so links find this instance.
fn node_name(opts: &OpenOptions) -> String {
    match opts.app {
        Some(_) => format!("{NODE_NAME}-{}", std::process::id()),
        None => NODE_NAME.to_string(),
    }
}

fn pw_dump() -> Result<serde_json::Value> {
    let out = std::process::Command::new("pw-dump")
        .output()
        .context("run pw-dump (is pipewire-utils installed?)")?;
    serde_json::from_slice(&out.stdout).context("parse pw-dump")
}

pub struct PipeWire;

/// PipeWire is running and handles audio: a daemon answers and has at
/// least one audio node. On a PulseAudio system it's either not running
/// or there only for screen sharing (Ubuntu 22.04), with no audio nodes;
/// then capture and playback go through ALSA instead (see
/// [`crate::cpal_input`]). `VOX_AUDIO_BACKEND=alsa` or `pipewire` skips
/// the check. Asked once per process.
pub fn running() -> bool {
    static RUNNING: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *RUNNING.get_or_init(|| {
        let forced = std::env::var("VOX_AUDIO_BACKEND").unwrap_or_default();
        let running = match forced.to_ascii_lowercase().as_str() {
            "alsa" => false,
            "pipewire" => true,
            _ => has_audio_nodes().unwrap_or(false),
        };
        info!(running, "PipeWire for audio");
        running
    })
}

/// Connects and lists the graph's nodes until the daemon has sent them all.
fn has_audio_nodes() -> Result<bool> {
    use std::cell::Cell;
    use std::rc::Rc;
    pw::init();
    let mainloop = MainLoop::new(None).context("MainLoop::new")?;
    let context = Context::new(&mainloop).context("Context::new")?;
    let core = context.connect(None).context("Context::connect")?;
    let registry = core.get_registry().context("get_registry")?;
    let found = Rc::new(Cell::new(false));
    let f = found.clone();
    let _globals = registry
        .add_listener_local()
        .global(move |g| {
            let class = g.props.and_then(|p| p.get("media.class"));
            if class.is_some_and(|c| c.starts_with("Audio/")) {
                f.set(true);
            }
        })
        .register();
    let pending = core.sync(0).context("sync")?;
    let ml = mainloop.clone();
    let _done = core
        .add_listener_local()
        .done(move |id, seq| {
            if id == pw::core::PW_ID_CORE && seq == pending {
                ml.quit();
            }
        })
        .register();
    mainloop.run();
    Ok(found.get())
}

impl Backend for PipeWire {
    fn name(&self) -> &'static str {
        "pipewire"
    }

    /// Lists sources and sinks via `pw-dump`. Pass a sink's name to
    /// capture its monitor (what's playing on it).
    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        let objects = pw_dump()?;
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
                loopback: false,
                id: name.to_string(),
                name: format!("{desc} ({kind})"),
                is_default: false,
                channels: pw_dump::layout_of(obj)
                    .map_or(0, |l| l.channels.min(u16::MAX as usize) as u16),
            });
        }
        Ok(devices)
    }

    /// App output streams, via `pw-dump`.
    fn list_apps(&self) -> Result<Vec<AppInfo>> {
        Ok(pw_dump::apps(&pw_dump()?))
    }

    fn open(&self, opts: &OpenOptions) -> Result<Capture> {
        if let Some(app) = &opts.app {
            if !self.list_apps()?.iter().any(|a| a.matches(app)) {
                anyhow::bail!("no PipeWire output stream matching {app:?} (try `apps`)");
            }
        }
        let layout = layout(opts)?;
        let mix = Mix::of(opts.channel, layout.channels).with_context(|| {
            format!(
                "capturing {}",
                opts.device.as_deref().unwrap_or("the default source")
            )
        })?;
        info!(
            channels = layout.channels,
            ?mix,
            "PipeWire capture channel layout"
        );
        let plumbing = Plumbing::new(RATE, layout.channels, mix);
        let (producer, parts) = plumbing.into_parts();
        let stop = parts.stop.clone();
        let overruns = parts.overruns.clone();
        let opts_t = opts.clone();
        let (ready_tx, ready_rx) = bounded::<Result<()>>(1);
        let thread = std::thread::Builder::new()
            .name("vox-pipewire".into())
            .spawn(move || {
                if let Err(err) = run(opts_t, layout, producer, stop, overruns, &ready_tx) {
                    error!(?err, "PipeWire capture thread exited with error");
                    let _ = ready_tx.send(Err(err));
                }
            })
            .context("spawn PipeWire thread")?;
        ready_rx.recv().context("PipeWire thread died")??;
        let label = match (&opts.app, &opts.device, opts.virtual_sink) {
            (Some(app), _, _) => format!("PipeWire app {app:?}"),
            (_, _, true) => format!("PipeWire sink '{NODE_NAME}'"),
            (_, Some(d), false) => opts.label(&format!("PipeWire {d}")),
            (_, None, false) => opts.label("PipeWire default source"),
        };
        Ok(parts.finish(RATE, label, thread))
    }
}

/// A single `MONO` channel: what app capture and the virtual sink want,
/// and the fallback when the graph won't say what a source runs in.
const MONO: Layout = Layout {
    channels: 1,
    positions: Vec::new(),
};

/// The channel layout to open the stream in.
///
/// App capture and the virtual sink are fed by links into our own node,
/// and PipeWire sums everything linked to one port: a single mono port
/// is right, and more would have every app port linked to every one of
/// ours and counted once per port. Only a source has a layout of its
/// own to match.
///
/// A source whose layout the graph won't say falls back to mono, as the
/// stream always asked for — unless one channel of it was asked for by
/// name, where guessing would transcribe the wrong mic.
fn layout(opts: &OpenOptions) -> Result<Layout> {
    if opts.app.is_some() || opts.virtual_sink {
        if opts.channel.is_some() {
            anyhow::bail!(
                "a single channel can only be taken from an input device, not from app capture or the virtual sink (both arrive already mixed)"
            );
        }
        return Ok(MONO);
    }
    let found = pw_dump().ok().and_then(|dump| {
        let node = match &opts.device {
            Some(device) => pw_dump::find_source(&dump, device)?,
            None => pw_dump::default_source(&dump)?.to_string(),
        };
        pw_dump::layout(&dump, &node)
    });
    match (found, opts.channel) {
        (Some(layout), _) => Ok(layout),
        (None, None) => Ok(MONO),
        (None, Some(_)) => anyhow::bail!(
            "can't tell how many channels {} has, so one channel of it can't be singled out",
            opts.device.as_deref().unwrap_or("the default source")
        ),
    }
}

/// A channel position as PipeWire names it (`FL`, `MONO`, `AUX7`), as
/// the SPA id a format pod wants. Mirroring the source's own positions
/// is what gets all of its ports linked to ours: the session manager
/// matches them up by position. Anything unrecognised becomes the `AUX`
/// channel at its own index, which links by order instead.
fn position_id(name: &str, index: usize) -> u32 {
    use libspa::sys as spa;
    let aux = |n: usize| spa::SPA_AUDIO_CHANNEL_AUX0 + n.min(63) as u32;
    match name.to_ascii_uppercase().as_str() {
        "MONO" => spa::SPA_AUDIO_CHANNEL_MONO,
        "FL" => spa::SPA_AUDIO_CHANNEL_FL,
        "FR" => spa::SPA_AUDIO_CHANNEL_FR,
        "FC" => spa::SPA_AUDIO_CHANNEL_FC,
        "LFE" => spa::SPA_AUDIO_CHANNEL_LFE,
        "SL" => spa::SPA_AUDIO_CHANNEL_SL,
        "SR" => spa::SPA_AUDIO_CHANNEL_SR,
        "FLC" => spa::SPA_AUDIO_CHANNEL_FLC,
        "FRC" => spa::SPA_AUDIO_CHANNEL_FRC,
        "RC" => spa::SPA_AUDIO_CHANNEL_RC,
        "RL" => spa::SPA_AUDIO_CHANNEL_RL,
        "RR" => spa::SPA_AUDIO_CHANNEL_RR,
        "TC" => spa::SPA_AUDIO_CHANNEL_TC,
        other => other
            .strip_prefix("AUX")
            .and_then(|n| n.parse::<usize>().ok())
            .map_or_else(|| aux(index), aux),
    }
}

fn run(
    opts: OpenOptions,
    layout: Layout,
    producer: rtrb::Producer<f32>,
    stop: Arc<AtomicBool>,
    overruns: Arc<AtomicU64>,
    ready: &crossbeam_channel::Sender<Result<()>>,
) -> Result<()> {
    pw::init();
    let mainloop = MainLoop::new(None).context("MainLoop::new")?;
    let context = Context::new(&mainloop).context("Context::new")?;
    let core = context.connect(None).context("Context::connect")?;

    let node_name = node_name(&opts);
    let mut props = properties! {
        *keys::MEDIA_TYPE => "Audio",
        *keys::MEDIA_CATEGORY => "Capture",
        *keys::MEDIA_ROLE => "Production",
        *keys::NODE_NAME => node_name.as_str(),
        *keys::NODE_DESCRIPTION => "vox_scribe transcription",
    };
    if opts.app.is_some() {
        // Linked by `link_app`, not the session manager.
    } else if opts.virtual_sink {
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

    let stream = Stream::new(&core, &node_name, props).context("Stream::new")?;

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
    info.set_channels(layout.channels as u32);
    // Only when the source named its channels: left unset, PipeWire
    // names a single channel MONO, which is what this always asked for
    // and what app capture and the virtual sink still want.
    if !layout.positions.is_empty() {
        let mut position = [0u32; 64];
        for (i, slot) in position
            .iter_mut()
            .enumerate()
            .take(layout.channels.min(64))
        {
            let name = layout.positions.get(i).map_or("", String::as_str);
            *slot = position_id(name, i);
        }
        info.set_position(position);
    }
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
    let mut flags = StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS;
    if opts.app.is_none() {
        flags |= StreamFlags::AUTOCONNECT;
    }
    stream
        .connect(libspa::utils::Direction::Input, None, flags, &mut params)
        .context("Stream::connect")?;
    let _ = ready.send(Ok(()));

    let linker = match &opts.app {
        Some(app) => {
            let (app, stop) = (app.clone(), stop.clone());
            Some(
                std::thread::Builder::new()
                    .name("vox-pipewire-link".into())
                    .spawn(move || link_app(&app, &node_name, &stop))
                    .context("spawn PipeWire linker")?,
            )
        }
        None => None,
    };

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
    if let Some(linker) = linker {
        let _ = linker.join();
    }
    Ok(())
}

/// Keep every output port of the apps matching `app` linked to our node.
fn link_app(app: &str, node_name: &str, stop: &AtomicBool) {
    let mut next = std::time::Instant::now();
    while !stop.load(Ordering::SeqCst) {
        if std::time::Instant::now() >= next {
            next = std::time::Instant::now() + RELINK;
            let dump = match pw_dump() {
                Ok(d) => d,
                Err(err) => {
                    error!(?err, "pw-dump failed");
                    continue;
                }
            };
            for (out, inp) in pw_dump::missing_links(&dump, app, node_name) {
                let status = std::process::Command::new("pw-link")
                    .arg(out.to_string())
                    .arg(inp.to_string())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
                match status {
                    Ok(s) if s.success() => info!(out, inp, "linked app port"),
                    other => error!(out, inp, ?other, "pw-link failed"),
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
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
