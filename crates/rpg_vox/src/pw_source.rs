//! Native PipeWire source node + graph client.
//!
//! Runs the PipeWire main loop on a dedicated OS thread. Two things live there:
//!
//! 1. A source stream that appears as `Audio/Source`. Its realtime `process`
//!    callback drains f32 mono samples from an `rtrb::Consumer` and writes them
//!    into the negotiated output buffer, emitting silence on underrun.
//!
//! 2. A registry listener that tracks the surrounding graph (sinks + our own
//!    ports + all links). The tokio side talks to the pw thread through a
//!    `pipewire::channel` and gets replies via `tokio::sync::oneshot`.

use anyhow::{Context as _, Result};
use rtrb::Consumer;
use serde::Serialize;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use tokio::sync::oneshot;
use tracing::{debug, error, info, warn};

use pipewire as pw;
use pw::{
    channel as pw_channel,
    context::Context,
    core::Core,
    keys,
    main_loop::MainLoop,
    properties::properties,
    registry::{GlobalObject, Registry},
    stream::{Stream, StreamFlags},
    types::ObjectType,
};

use libspa::{
    param::audio::{AudioFormat, AudioInfoRaw},
    pod::{Object, Pod, Value, serialize::PodSerializer},
    utils::dict::DictRef,
};

// -----------------------------------------------------------------------------
// Public API
// -----------------------------------------------------------------------------

pub struct Config {
    pub node_name: String,
    pub node_description: String,
    pub sample_rate: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct NodeInfo {
    pub id: u32,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Serialize)]
pub struct GraphSnapshot {
    pub own_node_id: Option<u32>,
    pub sinks: Vec<NodeInfo>,
    pub listeners: Vec<NodeInfo>,
    pub monitor_sink_id: Option<u32>,
    pub auto_patch_sink_id: Option<u32>,
}

pub enum Command {
    Snapshot(oneshot::Sender<GraphSnapshot>),
    StartMonitor {
        sink_id: u32,
        reply: oneshot::Sender<Result<(), String>>,
    },
    StopMonitor {
        reply: oneshot::Sender<Result<(), String>>,
    },
    StartAutoPatch {
        sink_id: u32,
        reply: oneshot::Sender<Result<(), String>>,
    },
}

#[derive(Clone)]
pub struct PwClient {
    tx: pw_channel::Sender<Command>,
}

impl PwClient {
    pub async fn snapshot(&self) -> Result<GraphSnapshot, String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::Snapshot(tx))
            .map_err(|_| "pw thread gone".to_string())?;
        rx.await.map_err(|_| "pw thread dropped reply".to_string())
    }

    pub async fn start_monitor(&self, sink_id: u32) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::StartMonitor {
                sink_id,
                reply: tx,
            })
            .map_err(|_| "pw thread gone".to_string())?;
        rx.await.map_err(|_| "pw thread dropped reply".to_string())?
    }

    pub async fn stop_monitor(&self) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::StopMonitor { reply: tx })
            .map_err(|_| "pw thread gone".to_string())?;
        rx.await.map_err(|_| "pw thread dropped reply".to_string())?
    }

    pub async fn start_auto_patch(&self, sink_id: u32) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::StartAutoPatch {
                sink_id,
                reply: tx,
            })
            .map_err(|_| "pw thread gone".to_string())?;
        rx.await.map_err(|_| "pw thread dropped reply".to_string())?
    }
}

pub struct Handle {
    pub client: PwClient,
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
    let (cmd_tx, cmd_rx) = pw_channel::channel::<Command>();

    let thread = thread::Builder::new()
        .name("pw-source".into())
        .spawn(move || {
            if let Err(err) = run(cfg, consumer, stop_thread, cmd_rx) {
                error!(?err, "PipeWire source thread exited with error");
            }
        })
        .context("failed to spawn PipeWire source thread")?;

    Ok(Handle {
        client: PwClient { tx: cmd_tx },
        stop,
        thread: Some(thread),
    })
}

// -----------------------------------------------------------------------------
// Internal graph state (lives inside the pw thread)
// -----------------------------------------------------------------------------

#[derive(Default)]
struct Graph {
    nodes: HashMap<u32, TrackedNode>,
    ports: HashMap<u32, TrackedPort>,
    links: HashMap<u32, TrackedLink>,
    own_node_id: Option<u32>,
    /// Debug listen-along link set, toggled by the UI.
    monitor: Option<ActiveLinkSet>,
    /// Production link set, established at startup from RPG_VOX_AUTO_LINK.
    auto_patch: Option<ActiveLinkSet>,
}

struct TrackedNode {
    name: String,
    description: String,
    media_class: String,
}

struct TrackedPort {
    node_id: u32,
    direction: PortDirection,
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum PortDirection {
    Input,
    Output,
}

struct TrackedLink {
    output_node: u32,
    input_node: u32,
}

struct ActiveLinkSet {
    sink_id: u32,
    /// Keeps the link proxies alive; dropping them removes the links.
    _links: Vec<pipewire::link::Link>,
}

impl Graph {
    fn our_output_port_ids(&self) -> Vec<u32> {
        let Some(own) = self.own_node_id else {
            return vec![];
        };
        self.ports
            .iter()
            .filter(|(_, p)| p.node_id == own && p.direction == PortDirection::Output)
            .map(|(id, _)| *id)
            .collect()
    }

    fn sink_input_port_ids(&self, sink_id: u32) -> Vec<u32> {
        self.ports
            .iter()
            .filter(|(_, p)| p.node_id == sink_id && p.direction == PortDirection::Input)
            .map(|(id, _)| *id)
            .collect()
    }

    fn snapshot(&self) -> GraphSnapshot {
        let mut sinks: Vec<NodeInfo> = self
            .nodes
            .iter()
            .filter(|(_, n)| n.media_class == "Audio/Sink")
            .map(|(id, n)| NodeInfo {
                id: *id,
                name: n.name.clone(),
                description: n.description.clone(),
            })
            .collect();
        sinks.sort_by(|a, b| a.description.cmp(&b.description));

        let mut listeners: Vec<NodeInfo> = Vec::new();
        if let Some(own) = self.own_node_id {
            let mut seen: std::collections::HashSet<u32> = Default::default();
            for link in self.links.values() {
                if link.output_node != own {
                    continue;
                }
                let target = link.input_node;
                if !seen.insert(target) {
                    continue;
                }
                if let Some(node) = self.nodes.get(&target) {
                    listeners.push(NodeInfo {
                        id: target,
                        name: node.name.clone(),
                        description: node.description.clone(),
                    });
                }
            }
            listeners.sort_by(|a, b| a.description.cmp(&b.description));
        }

        GraphSnapshot {
            own_node_id: self.own_node_id,
            sinks,
            listeners,
            monitor_sink_id: self.monitor.as_ref().map(|m| m.sink_id),
            auto_patch_sink_id: self.auto_patch.as_ref().map(|p| p.sink_id),
        }
    }
}

// -----------------------------------------------------------------------------
// Registry callbacks
// -----------------------------------------------------------------------------

fn dict_get<'a>(props: Option<&'a DictRef>, key: &str) -> Option<&'a str> {
    props.and_then(|d| d.get(key))
}

fn on_global(graph: &Rc<RefCell<Graph>>, obj: &GlobalObject<&DictRef>) {
    let id = obj.id;
    match obj.type_ {
        ObjectType::Node => {
            let media_class = dict_get(obj.props, "media.class").unwrap_or("").to_string();
            let name = dict_get(obj.props, *keys::NODE_NAME).unwrap_or("").to_string();
            let description = dict_get(obj.props, *keys::NODE_DESCRIPTION)
                .unwrap_or(&name)
                .to_string();
            graph.borrow_mut().nodes.insert(
                id,
                TrackedNode {
                    name,
                    description,
                    media_class,
                },
            );
        }
        ObjectType::Port => {
            let node_id = dict_get(obj.props, "node.id")
                .and_then(|s| s.parse::<u32>().ok());
            let dir = dict_get(obj.props, "port.direction");
            if let (Some(node_id), Some(dir)) = (node_id, dir) {
                let direction = if dir == "out" {
                    PortDirection::Output
                } else {
                    PortDirection::Input
                };
                graph
                    .borrow_mut()
                    .ports
                    .insert(id, TrackedPort { node_id, direction });
            }
        }
        ObjectType::Link => {
            let out_node = dict_get(obj.props, "link.output.node")
                .and_then(|s| s.parse::<u32>().ok());
            let in_node = dict_get(obj.props, "link.input.node")
                .and_then(|s| s.parse::<u32>().ok());
            if let (Some(o), Some(i)) = (out_node, in_node) {
                graph.borrow_mut().links.insert(
                    id,
                    TrackedLink {
                        output_node: o,
                        input_node: i,
                    },
                );
            }
        }
        _ => {}
    }
}

fn on_global_remove(graph: &Rc<RefCell<Graph>>, id: u32) {
    let mut g = graph.borrow_mut();
    g.nodes.remove(&id);
    g.ports.remove(&id);
    g.links.remove(&id);
}

// -----------------------------------------------------------------------------
// Command handling
// -----------------------------------------------------------------------------

fn handle_command(core: &Core, graph: &Rc<RefCell<Graph>>, cmd: Command) {
    match cmd {
        Command::Snapshot(reply) => {
            let snap = graph.borrow().snapshot();
            let _ = reply.send(snap);
        }
        Command::StartMonitor { sink_id, reply } => {
            let result = start_monitor(core, graph, sink_id);
            let _ = reply.send(result.map_err(|e| format!("{e:#}")));
        }
        Command::StopMonitor { reply } => {
            graph.borrow_mut().monitor = None;
            let _ = reply.send(Ok(()));
        }
        Command::StartAutoPatch { sink_id, reply } => {
            let result = start_auto_patch(core, graph, sink_id);
            let _ = reply.send(result.map_err(|e| format!("{e:#}")));
        }
    }
}

/// Create one link from every source output port to every sink input port
/// (mono → N-channel is fan-out — same signal on L and R for stereo sinks).
/// Returns the sink's description and the Link handles that must be kept
/// alive to hold the links open.
fn create_links_to_sink(
    core: &Core,
    graph: &Rc<RefCell<Graph>>,
    sink_id: u32,
) -> anyhow::Result<(String, Vec<pipewire::link::Link>)> {
    let g = graph.borrow();

    let sink = g
        .nodes
        .get(&sink_id)
        .ok_or_else(|| anyhow::anyhow!("sink id {sink_id} not in graph"))?;
    anyhow::ensure!(
        sink.media_class == "Audio/Sink",
        "target id {sink_id} is not an Audio/Sink"
    );
    let sink_desc = sink.description.clone();

    let own = g
        .own_node_id
        .ok_or_else(|| anyhow::anyhow!("own source node not yet registered"))?;
    let out_ports = g.our_output_port_ids();
    anyhow::ensure!(
        !out_ports.is_empty(),
        "our source has no output ports yet — try again in a moment"
    );
    let in_ports = g.sink_input_port_ids(sink_id);
    anyhow::ensure!(
        !in_ports.is_empty(),
        "sink {sink_desc} has no input ports"
    );

    drop(g);

    let mut created: Vec<pipewire::link::Link> = Vec::new();
    for &in_port in &in_ports {
        for &out_port in &out_ports {
            let props = properties! {
                "link.output.node" => own.to_string(),
                "link.output.port" => out_port.to_string(),
                "link.input.node"  => sink_id.to_string(),
                "link.input.port"  => in_port.to_string(),
                "object.linger" => "false",
            };
            let link: pipewire::link::Link = core
                .create_object("link-factory", &props)
                .with_context(|| {
                    format!("create link {own}:{out_port} -> {sink_id}:{in_port}")
                })?;
            created.push(link);
        }
    }

    Ok((sink_desc, created))
}

fn start_monitor(
    core: &Core,
    graph: &Rc<RefCell<Graph>>,
    sink_id: u32,
) -> anyhow::Result<()> {
    let (sink_desc, links) = create_links_to_sink(core, graph, sink_id)?;
    graph.borrow_mut().monitor = Some(ActiveLinkSet {
        sink_id,
        _links: links,
    });
    info!(sink = %sink_desc, "monitor started");
    Ok(())
}

fn start_auto_patch(
    core: &Core,
    graph: &Rc<RefCell<Graph>>,
    sink_id: u32,
) -> anyhow::Result<()> {
    let (sink_desc, links) = create_links_to_sink(core, graph, sink_id)?;
    graph.borrow_mut().auto_patch = Some(ActiveLinkSet {
        sink_id,
        _links: links,
    });
    info!(sink = %sink_desc, "auto-patch established");
    Ok(())
}

// -----------------------------------------------------------------------------
// PipeWire main-thread entrypoint
// -----------------------------------------------------------------------------

fn run(
    cfg: Config,
    consumer: Consumer<f32>,
    stop: Arc<AtomicBool>,
    cmd_rx: pw_channel::Receiver<Command>,
) -> Result<()> {
    pw::init();

    let mainloop = MainLoop::new(None).context("MainLoop::new")?;
    let context = Context::new(&mainloop).context("Context::new")?;
    let core = context.connect(None).context("Context::connect")?;

    let graph: Rc<RefCell<Graph>> = Rc::new(RefCell::new(Graph::default()));

    // Registry listener.
    let registry: Registry = core.get_registry().context("get_registry")?;
    let graph_g = graph.clone();
    let graph_r = graph.clone();
    let _registry_listener = registry
        .add_listener_local()
        .global(move |obj| on_global(&graph_g, obj))
        .global_remove(move |id| on_global_remove(&graph_r, id))
        .register();

    // Source stream.
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
        underruns: u64,
    }
    let state = StreamState {
        consumer,
        underruns: 0,
    };

    let graph_stream = graph.clone();
    let _listener = stream
        .add_local_listener_with_user_data::<StreamState>(state)
        .state_changed(move |stream, _, old, new| {
            info!(?old, ?new, "PipeWire stream state changed");
            // Record our own global node id as soon as it's available.
            let id = stream.node_id();
            if id != u32::MAX {
                graph_stream.borrow_mut().own_node_id = Some(id);
            }
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

            let frames_written = if let Some(slice) = data.data() {
                let out: &mut [f32] = bytemuck_cast_slice_mut(slice);
                let mut written = 0usize;
                while written < out.len() {
                    match state.consumer.pop() {
                        Ok(sample) => {
                            out[written] = sample;
                            written += 1;
                        }
                        Err(_) => break,
                    }
                }
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
                out.len()
            } else {
                0
            };

            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = stride as _;
            *chunk.size_mut() = (stride * frames_written) as _;
        })
        .register()
        .context("Stream::register")?;

    // Format param.
    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(cfg.sample_rate);
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
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
            &mut params,
        )
        .context("Stream::connect")?;

    // Command receiver, attached to the pw main loop so incoming commands wake
    // the loop and are dispatched on this thread.
    let core_cmd = core.clone();
    let graph_cmd = graph.clone();
    let _cmd_receiver = cmd_rx.attach(mainloop.loop_(), move |cmd| {
        handle_command(&core_cmd, &graph_cmd, cmd);
    });

    // Periodic timer to poll the stop flag.
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

/// Reinterpret a byte slice as an f32 slice. PipeWire aligns buffers for the
/// negotiated F32LE sample format.
fn bytemuck_cast_slice_mut(bytes: &mut [u8]) -> &mut [f32] {
    let len = bytes.len() / std::mem::size_of::<f32>();
    let ptr = bytes.as_mut_ptr() as *mut f32;
    unsafe { std::slice::from_raw_parts_mut(ptr, len) }
}
