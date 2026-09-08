//! Native PipeWire source node + graph client.
//!
//! Runs the PipeWire main loop on a dedicated OS thread. Three things live there:
//!
//! 1. A source stream that appears as a stereo `Audio/Source`. Its realtime
//!    `process` callback drains stereo `[L, R]` pairs from three
//!    `rtrb::Consumer`s (TTS, music, vox), applies per-strip pan/gain/mute
//!    per-channel, sums them into the negotiated interleaved output buffer,
//!    and emits silence on underrun. Downstream FX / panning always operate
//!    on the stereo pair — nothing in the chain collapses to mono.
//!
//! 2. Two companion sink streams — one per fixed [`SinkRole`]:
//!    * `{node_name}-music` (`Audio/Sink`, `media.role = Music`) — each
//!      incoming stereo frame is pushed verbatim into an SPSC ring drained
//!      by the source callback, which mixes it with TTS to build the final
//!      mic feed. A PA that speaks into Discord.
//!    * `{node_name}-vox` (`Audio/Sink`, `media.role = Communication`) —
//!      same passthrough plus a broadcast tap ([`Config::input_tap`]) of
//!      the raw interleaved stereo frame for internal consumers (future
//!      FX chain, recording, an input-side browser monitor).
//!
//!    Each role has its own dedicated node so external apps route by name
//!    instead of toggling a runtime mode. Nodes always exist even with
//!    nothing connected — idle nodes are cheap.
//!
//!    Two separate nodes (rather than one node with mixed-direction ports)
//!    is a `pipewire-rs` 0.8 constraint: it doesn't wrap `pw_filter`, which
//!    is the primitive for a single node with both input and output ports.
//!    Migration to a filter-based single node is a possible future FFI
//!    project.
//!
//! 3. A registry listener that tracks the surrounding graph (sinks + our own
//!    ports + all links). The tokio side talks to the pw thread through a
//!    `pipewire::channel` and gets replies via `tokio::sync::oneshot`.

use anyhow::{Context as _, Result};
use rtrb::{Consumer, Producer, RingBuffer};
use serde::Serialize;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use tokio::sync::{broadcast, oneshot};
use tracing::{debug, error, info, warn};

use crate::mixer::AtomicMixer;

use pipewire as pw;
use pw::{
    channel as pw_channel,
    context::Context,
    core::Core,
    keys,
    main_loop::MainLoop,
    metadata::{Metadata, MetadataListener},
    node::{Node as PwNode, NodeListener},
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

/// Channel count offered to PipeWire. Mono TTS gets fanned out to L=R here so
/// downstream FX / panning always operate on a stereo pair.
const SOURCE_CHANNELS: u32 = 2;

/// Marker string embedded (via zero-width spaces) in the SPA's
/// `<title>` element. Firefox propagates `document.title` to the pipewire
/// node's `media.name` for streams a page creates (Web Audio, MediaStream),
/// so any pipewire stream whose media.name *contains* this substring is
/// coming from our own Mixer tab and would produce an immediate feedback
/// loop if routed into the mix. We match by substring rather than
/// equality because the marker sits alongside the user-visible title.
///
/// Keep in sync with the `<title>` in
/// `crates/rpg_vox/web/index.html`.
const BROWSER_MONITOR_STREAM_TITLE: &str = "rpg-vox-browser-monitor";
/// Stereo input on the companion sink node. Matches [`SOURCE_CHANNELS`] so
/// eventual mix-into-output paths don't need channel-count coercion.
const SINK_CHANNELS: u32 = 2;

/// Fixed role for a companion sink node. Each role has its own dedicated
/// PipeWire node so external apps route into the "music" sink when they
/// want their audio mixed straight into the mic (PA), and into the "vox"
/// sink when they want it captured for internal processing (FX chain,
/// recording, etc.). Nodes always exist even with nothing connected —
/// leaving them idle is cheap.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SinkRole {
    /// PA passthrough — downmix stereo to mono, push into the input ring,
    /// summed with TTS in the source callback.
    Music,
    /// Internal processing tap — publish stereo samples to the broadcast
    /// channel so future consumers (FX, recording, monitor) can subscribe.
    Vox,
}

impl SinkRole {
    pub fn suffix(self) -> &'static str {
        match self {
            Self::Music => "music",
            Self::Vox => "vox",
        }
    }
    fn description(self) -> &'static str {
        match self {
            Self::Music => "music (PA \u{2192} mic)",
            Self::Vox => "vox (processing)",
        }
    }
    fn media_role(self) -> &'static str {
        match self {
            Self::Music => "Music",
            Self::Vox => "Communication",
        }
    }
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "music" => Some(Self::Music),
            "vox" => Some(Self::Vox),
            _ => None,
        }
    }
}

pub struct Config {
    pub node_name: String,
    pub node_description: String,
    pub sample_rate: u32,
    /// `node.name` we should auto-patch our source into on every appearance.
    /// Exposed in `GraphSnapshot` so the UI can hide the target from the
    /// monitor picker even while a reconnection is still in flight.
    pub auto_patch_target: Option<String>,
    /// Broadcast sender for the `-vox` sink's processing tap. Consumers
    /// subscribe on demand; when there are none the send is a cheap no-op.
    /// Interleaved stereo f32 at `sample_rate` (mirrors the wire format of
    /// the sink node).
    pub input_tap: broadcast::Sender<Arc<[f32]>>,
    /// Broadcast sender for the browser-monitor PCM tap. Fired from the
    /// source stream's process callback with the final interleaved stereo
    /// (L, R) frames that are about to be handed to pipewire — so
    /// `/monitor.ws` subscribers hear the actual mic feed with pan and
    /// per-strip gain applied. Zero subscribers makes `send` a cheap no-op.
    pub monitor_tap: broadcast::Sender<Arc<[f32]>>,
    /// Shared mixer state. The source callback reads gain/pan/mute per
    /// channel every process cycle (lock-free atomics) to compute the
    /// stereo mic feed.
    pub mixer: Arc<AtomicMixer>,
}

#[derive(Clone, Debug, Serialize)]
pub struct NodeInfo {
    pub id: u32,
    pub name: String,
    pub description: String,
}

/// An audio producer visible in the graph. Two flavours:
///   * `kind = "stream"` — `Stream/Output/Audio`, an app's output stream
///     (Firefox tab, mpv, etc.). Its natural home is the default sink;
///     rpg_vox can override that with WP metadata.
///   * `kind = "device"` — `Audio/Source`, a physical capture device (USB
///     mic, line-in). It has no default sink routing of its own — the UI
///     treats "unrouted" as *not fed into rpg_vox*, since nobody wants a
///     mic mirrored to the speakers as a default.
///
/// Split off from [`NodeInfo`] so the client can render app + per-stream
/// title separately (crucial when three "Firefox" nodes exist for three
/// different tabs).
#[derive(Clone, Debug, Serialize)]
pub struct SourceInfo {
    pub id: u32,
    /// `node.name` — usually the app's own tag ("Firefox", "mpv") or, for
    /// devices, the raw ALSA node name. Often non-unique across the graph.
    pub name: String,
    /// `node.description` — human-readable label. For devices this is the
    /// friendly name ("Blue Yeti"); for streams it's often the app name.
    pub description: String,
    /// `"stream"` for app outputs, `"device"` for physical capture nodes.
    /// The Mixer uses this to change the third routing button's semantics
    /// (streams get "Default" = restore to default sink; devices get "Off"
    /// = don't feed rpg_vox).
    pub kind: String,
    /// `application.name` — human-readable app label. Preferred over `name`
    /// in the UI for streams. Absent for devices and a few oddball producers.
    pub application_name: Option<String>,
    /// `media.name` — per-stream label the app sets. For Firefox this is
    /// the tab title; for mpv it's the file name; for random WebRTC
    /// streams it's often just "AudioStream". Absent for devices.
    pub media_name: Option<String>,
    /// `application.icon-name` — freedesktop icon key if the app set one.
    pub icon_name: Option<String>,
    /// `application.process.id` — pid of the producing app, when known.
    /// Handy for grouping streams that all come from the same browser
    /// process in the UI. Absent for devices.
    pub pid: Option<u32>,
    /// Which of our companion sinks (if any) this source is currently
    /// linked to. Ground truth from the link graph: if the source feeds
    /// our music/vox sink by any means (this session, a previous run,
    /// pactl, crosspipe...) that's what the toggle reflects.
    pub routed_to: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct GraphSnapshot {
    pub own_node_id: Option<u32>,
    pub sinks: Vec<NodeInfo>,
    pub listeners: Vec<NodeInfo>,
    pub monitor_sink_id: Option<u32>,
    pub auto_patch_sink_id: Option<u32>,
    /// Configured auto-patch target `node.name`, if any. Stable across
    /// peer restarts (whereas `auto_patch_sink_id` clears while the peer
    /// is gone).
    pub auto_patch_target: Option<String>,
    /// External `Stream/Output/Audio` producers currently visible in the
    /// graph. The Mixer UI lists these so the user can pick which
    /// same-named tab (e.g. Firefox) to route into which companion sink.
    pub sources: Vec<SourceInfo>,
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
    /// Link an external `Stream/Output/Audio` node into one of our
    /// companion sinks. If the source was already routed by us to a
    /// different (or the same) sink, the previous link set is dropped
    /// first so we don't double-feed the mix.
    LinkSourceToSink {
        source_id: u32,
        target: SinkRole,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Drop the link set (if any) that we previously created for this
    /// source. Idempotent: unrouted sources return Ok(()).
    UnlinkSource {
        source_id: u32,
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

    pub async fn link_source(&self, source_id: u32, target: SinkRole) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::LinkSourceToSink {
                source_id,
                target,
                reply: tx,
            })
            .map_err(|_| "pw thread gone".to_string())?;
        rx.await.map_err(|_| "pw thread dropped reply".to_string())?
    }

    pub async fn unlink_source(&self, source_id: u32) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Command::UnlinkSource {
                source_id,
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

pub fn spawn(cfg: Config, consumer: Consumer<[f32; 2]>) -> Result<Handle> {
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
    /// Production link set. Reset to `None` when the target sink disappears
    /// so the auto-patch task can re-establish it on reconnect.
    auto_patch: Option<ActiveLinkSet>,
    /// Configured auto-patch target `node.name`, if any. Set once on startup.
    auto_patch_target: Option<String>,
    /// `node.name` of our companion `-music` sink. Set once on startup so
    /// we can look up its id in `nodes` at link time (it appears whenever
    /// the sink stream registers).
    music_sink_name: String,
    /// `node.name` of our companion `-vox` sink. Same as above.
    vox_sink_name: String,
    // (Routed-source tracking removed: `routed_to` in SourceInfo is now
    // derived directly from the link graph — see `source_routed_to`.
    // Metadata cleanup on node disappearance is unconditional because
    // pipewire recycles node ids, and any stale `target.object` sitting
    // on a vanished id would silently re-route the next node to inherit
    // that id.)
    /// Bound proxy for the `default` metadata object. Owning this proxy
    /// keeps the connection open so `set_property` writes actually apply.
    /// `None` while pipewire hasn't announced the metadata yet — routing
    /// commands issued before that fail with a clear error, but in
    /// practice WP is up before we finish enumerating the registry.
    default_metadata: Option<Metadata>,
    /// Listener on the `default` metadata. We only need it kept alive so
    /// the proxy stays subscribed; we don't currently react to WP-side
    /// property changes (a future refinement could reconcile
    /// `routed_sources` when an external tool touches the metadata).
    _default_metadata_listener: Option<MetadataListener>,
    /// Explicit link sets we've created to route hardware `Audio/Source`
    /// nodes (USB mics, line-in, ...) into one of our companion sinks.
    /// Keyed by the source node id so a re-route (Music → Vox) drops the
    /// prior set before creating the new one, and an Off/unroute simply
    /// removes the entry. Hardware sources can't be steered with WP's
    /// `target.object` metadata (that key only affects `Stream/*` policy),
    /// so we create pipewire links directly — the same mechanism used by
    /// the debug monitor and auto-patch paths.
    device_links: HashMap<u32, ActiveLinkSet>,
    /// Bound `Node` proxies for the producer streams we care about. The
    /// initial registry `global` dict only carries a subset of a node's
    /// properties (`application.name`, `node.name`, `media.class` — but
    /// *not* `media.name`, which is what distinguishes sibling Firefox
    /// tabs). Binding a proxy lets us subscribe to `info` events, which
    /// deliver the full props dict — we merge those into the TrackedNode
    /// so the Mixer's Sources panel can label rows by tab title.
    ///
    /// Both the proxy and its listener must stay alive; dropping either
    /// severs the subscription. Cleaned up in `on_global_remove`.
    bound_nodes: HashMap<u32, BoundNode>,
}

struct TrackedNode {
    name: String,
    description: String,
    media_class: String,
    application_name: Option<String>,
    media_name: Option<String>,
    icon_name: Option<String>,
    pid: Option<u32>,
    /// `object.serial` — a per-session stable id that survives node
    /// renames but is regenerated on process restart. Required as the
    /// value of the `target.object` metadata key when we want
    /// WirePlumber to route into one of our sinks (WP's linking policy
    /// only respects that key when the value is typed as `Spa:Id` and
    /// carries the target's serial, not its node id or name).
    object_serial: Option<u32>,
}

struct BoundNode {
    /// The bound Node proxy. Held to keep the server-side binding alive
    /// so `info` events keep flowing.
    _proxy: PwNode,
    /// The info listener. Dropping this removes the callback registration.
    _listener: NodeListener,
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

        let mut sources: Vec<SourceInfo> = self
            .nodes
            .iter()
            .filter(|(_, n)| {
                n.media_class == "Stream/Output/Audio" || n.media_class == "Audio/Source"
            })
            // Hide our own virtual mic (`Audio/Source`). Routing it into a
            // companion sink would create a feedback loop — the sink is
            // summed back into this exact source in the mix callback.
            .filter(|(id, _)| Some(*id) != self.own_node_id.as_ref())
            // Hide our own browser monitor. Firefox uses document.title as
            // the pipewire stream's media.name, so we match a substring
            // marker planted in the SPA's <title> (see index.html).
            .filter(|(_, n)| {
                !n.media_name
                    .as_deref()
                    .is_some_and(|m| m.contains(BROWSER_MONITOR_STREAM_TITLE))
            })
            .map(|(id, n)| SourceInfo {
                id: *id,
                name: n.name.clone(),
                description: n.description.clone(),
                kind: if n.media_class == "Audio/Source" {
                    "device".to_string()
                } else {
                    "stream".to_string()
                },
                application_name: n.application_name.clone(),
                media_name: n.media_name.clone(),
                icon_name: n.icon_name.clone(),
                pid: n.pid,
                // Ground truth: derive from actual outgoing links. If the
                // source is currently wired into one of our companion
                // sinks — whether by *this* process, a previous run,
                // pactl, crosspipe, or anything else — that's what the
                // toggle should reflect. Reading from the local
                // `routed_sources` map (what *we* set this session) would
                // desync the UI whenever the routing was established by
                // something we didn't own.
                routed_to: self.source_routed_to(*id),
            })
            .collect();
        // Group by app name so all Firefox tabs cluster together, then by
        // media.name so the order is stable across polls (falls back to
        // node id when both are absent — rare, but keeps ordering total).
        sources.sort_by(|a, b| {
            a.application_name
                .as_deref()
                .unwrap_or(&a.name)
                .cmp(b.application_name.as_deref().unwrap_or(&b.name))
                .then_with(|| {
                    a.media_name
                        .as_deref()
                        .unwrap_or("")
                        .cmp(b.media_name.as_deref().unwrap_or(""))
                })
                .then(a.id.cmp(&b.id))
        });

        GraphSnapshot {
            own_node_id: self.own_node_id,
            sinks,
            listeners,
            monitor_sink_id: self.monitor.as_ref().map(|m| m.sink_id),
            auto_patch_sink_id: self.auto_patch.as_ref().map(|p| p.sink_id),
            auto_patch_target: self.auto_patch_target.clone(),
            sources,
        }
    }

    /// Determine which of our companion sinks (if any) this source is
    /// currently linked to, by walking the tracked links. Returns the
    /// role's suffix ("music" / "vox") to serialize straight into the
    /// SourceInfo's `routed_to` string. Returns `None` for a source
    /// wired anywhere else (default sink, another app, nowhere).
    ///
    /// We check ALL outgoing links — if any of them lands on our sink,
    /// that counts as "routed to us". Ground-truth semantics: WP may
    /// briefly leave a stale link in place while it processes a routing
    /// change, but as soon as the routing has settled the reading here
    /// matches what the user hears.
    fn source_routed_to(&self, source_id: u32) -> Option<String> {
        for link in self.links.values() {
            if link.output_node != source_id {
                continue;
            }
            // Continue on missing target rather than short-circuit — a
            // link whose input node isn't in our nodes map yet just
            // isn't one of ours, so keep scanning the rest.
            let Some(target) = self.nodes.get(&link.input_node) else {
                continue;
            };
            if target.name == self.music_sink_name {
                return Some(SinkRole::Music.suffix().to_string());
            }
            if target.name == self.vox_sink_name {
                return Some(SinkRole::Vox.suffix().to_string());
            }
        }
        None
    }
}

// -----------------------------------------------------------------------------
// Registry callbacks
// -----------------------------------------------------------------------------

fn dict_get<'a>(props: Option<&'a DictRef>, key: &str) -> Option<&'a str> {
    props.and_then(|d| d.get(key))
}

fn on_global(
    graph: &Rc<RefCell<Graph>>,
    registry: &Rc<Registry>,
    obj: &GlobalObject<&DictRef>,
) {
    let id = obj.id;
    match obj.type_ {
        ObjectType::Node => {
            let media_class = dict_get(obj.props, "media.class").unwrap_or("").to_string();
            let name = dict_get(obj.props, *keys::NODE_NAME).unwrap_or("").to_string();
            let description = dict_get(obj.props, *keys::NODE_DESCRIPTION)
                .unwrap_or(&name)
                .to_string();
            // Optional metadata used by the Mixer's Sources panel to
            // disambiguate same-named nodes (three "Firefox" tabs, etc.).
            // The initial global carries `application.name` but not
            // `media.name` — the latter arrives on the node's info event
            // once we bind (see below).
            let application_name = dict_get(obj.props, "application.name").map(str::to_string);
            let media_name = dict_get(obj.props, "media.name").map(str::to_string);
            let icon_name = dict_get(obj.props, "application.icon-name").map(str::to_string);
            let pid = dict_get(obj.props, "application.process.id")
                .and_then(|s| s.parse::<u32>().ok());
            // `object.serial` is present in the initial global for every
            // node; capture it here so we can hand it to WP's
            // `target.object` metadata later without needing to bind and
            // wait for an info event.
            let object_serial = dict_get(obj.props, "object.serial")
                .and_then(|s| s.parse::<u32>().ok());
            let should_bind = media_class == "Stream/Output/Audio";
            graph.borrow_mut().nodes.insert(
                id,
                TrackedNode {
                    name,
                    description,
                    media_class,
                    application_name,
                    media_name,
                    icon_name,
                    pid,
                    object_serial,
                },
            );
            if should_bind {
                // Bind a proxy so we can subscribe to the node's info
                // events, which carry the full property dict (media.name
                // in particular). Silently ignore bind failures — nodes
                // occasionally disappear between announcement and bind,
                // and the initial-dict fields are usable on their own.
                if let Ok(proxy) = registry.bind::<PwNode, _>(obj) {
                    let graph_for_info = graph.clone();
                    let listener = proxy
                        .add_listener_local()
                        .info(move |info| {
                            if let Some(props) = info.props() {
                                merge_node_info(&graph_for_info, id, props);
                            }
                        })
                        .register();
                    graph.borrow_mut().bound_nodes.insert(
                        id,
                        BoundNode {
                            _proxy: proxy,
                            _listener: listener,
                        },
                    );
                }
            }
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
        ObjectType::Metadata => {
            // Bind the `default` metadata object so we can write
            // `target.object` on it to drive WirePlumber's routing. Other
            // metadata objects (settings, sm-settings, ...) are ignored.
            if dict_get(obj.props, "metadata.name") == Some("default")
                && graph.borrow().default_metadata.is_none()
            {
                if let Ok(proxy) = registry.bind::<Metadata, _>(obj) {
                    // Keep an (unused) empty listener registered so the
                    // proxy has a hook installed; some session managers
                    // consider a proxy without a listener as inactive.
                    let listener = proxy.add_listener_local().register();
                    let mut g = graph.borrow_mut();
                    g.default_metadata = Some(proxy);
                    g._default_metadata_listener = Some(listener);
                }
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
    // If the disappearing global was our patched-into sink, drop the link
    // handles so a new appearance is treated as a fresh peer.
    if g.monitor.as_ref().map(|m| m.sink_id) == Some(id) {
        g.monitor = None;
    }
    if g.auto_patch.as_ref().map(|p| p.sink_id) == Some(id) {
        g.auto_patch = None;
    }
    // Drop any device→sink link set whose device disappeared, and any
    // whose destination sink disappeared. In both cases the link proxies
    // are already dead (pipewire tore the links when the endpoint went
    // away); we're just releasing our handles so `routed_to` reads clean.
    g.device_links.remove(&id);
    g.device_links.retain(|_, ls| ls.sink_id != id);
    // Pipewire recycles node ids, and WP's `target.object` metadata is
    // keyed by subject id — leaving any stale routing keys behind causes
    // future nodes assigned this id to inherit the old routing, which
    // manifested as fresh browser-monitor streams silently getting
    // grabbed by rpg-vox-music after a tab reload. Clear unconditionally:
    // we don't care whether we set the metadata ourselves, pactl set it,
    // or crosspipe set it — once the subject is gone, its routing
    // preferences should not follow the recycled id.
    if let Some(metadata) = g.default_metadata.as_ref() {
        metadata.set_property(id, "target.object", None, None);
        metadata.set_property(id, "target.node", None, None);
    }
    // Drop any bound proxy + info listener for the node.
    g.bound_nodes.remove(&id);
}

/// Merge the full props dict (delivered by a node `info` event) into the
/// already-tracked node. Called at least once per bound node when the
/// initial info arrives, and again every time the app updates any of its
/// stream properties (Firefox does this when the tab title changes).
///
/// Side effect: if the incoming media.name identifies this node as our
/// own browser-monitor stream, scrub any `target.object`/`target.node`
/// metadata for it. Pipewire reuses node ids across appearances and WP's
/// metadata is keyed by subject id, so a reload of the mixer tab can
/// land the fresh monitor on an id that inherited stale routing from
/// something the user pinned earlier. We know these streams must never
/// be routed into our own sinks (they *are* the monitor), so it's safe
/// to unconditionally clear their overrides here.
fn merge_node_info(graph: &Rc<RefCell<Graph>>, id: u32, props: &DictRef) {
    let mut g = graph.borrow_mut();
    let Some(node) = g.nodes.get_mut(&id) else {
        return;
    };
    if let Some(v) = props.get("media.name") {
        node.media_name = Some(v.to_string());
    }
    if let Some(v) = props.get("application.name") {
        node.application_name = Some(v.to_string());
    }
    if let Some(v) = props.get("application.icon-name") {
        node.icon_name = Some(v.to_string());
    }
    if let Some(v) = props.get("application.process.id") {
        if let Ok(p) = v.parse::<u32>() {
            node.pid = Some(p);
        }
    }
    if let Some(v) = props.get(*keys::NODE_DESCRIPTION) {
        node.description = v.to_string();
    }
    // Scrub any stale target.object routing on the browser monitor's
    // stream. The monitor must never be routed into our own sinks (it
    // *is* the monitor); this defends against a fresh monitor node
    // inheriting stale metadata from a recycled id.
    let is_monitor = node
        .media_name
        .as_deref()
        .is_some_and(|m| m.contains(BROWSER_MONITOR_STREAM_TITLE));
    if is_monitor {
        if let Some(metadata) = g.default_metadata.as_ref() {
            metadata.set_property(id, "target.object", None, None);
            metadata.set_property(id, "target.node", None, None);
        }
    }
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
        Command::LinkSourceToSink {
            source_id,
            target,
            reply,
        } => {
            let result = match source_kind(graph, source_id) {
                Ok(SourceKind::Device) => link_device_source(core, graph, source_id, target),
                Ok(SourceKind::Stream) => {
                    route_source_via_metadata(graph, source_id, Some(target))
                }
                Err(e) => Err(e),
            };
            let _ = reply.send(result.map_err(|e| format!("{e:#}")));
        }
        Command::UnlinkSource { source_id, reply } => {
            let result = match source_kind(graph, source_id) {
                Ok(SourceKind::Device) => {
                    // Hardware sources never had metadata written for them
                    // — "Off" just means: drop the pipewire links we made.
                    // Idempotent: removing a missing entry is a no-op.
                    graph.borrow_mut().device_links.remove(&source_id);
                    Ok(())
                }
                Ok(SourceKind::Stream) => route_source_via_metadata(graph, source_id, None),
                Err(e) => Err(e),
            };
            let _ = reply.send(result.map_err(|e| format!("{e:#}")));
        }
    }
}

/// Pair a source node's output ports with a sink node's input ports and
/// create one PipeWire link per pair. Port IDs are sorted so pairing is
/// stable across restarts, and PipeWire assigns port IDs in
/// channel-position order (FL before FR), so a sorted 1:1 mapping preserves
/// stereo channels.
///
/// Fallback strategies handle mismatched counts:
///   * source=1 → fan out to every sink input (mono source, N-channel sink)
///   * sink=1   → fan in every source output to the one sink input
///   * N == M   → pair by index
///   * mismatch → warn and fan out fully (last-resort mesh)
fn create_links(
    core: &Core,
    graph: &Rc<RefCell<Graph>>,
    source_id: u32,
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

    anyhow::ensure!(
        g.nodes.contains_key(&source_id),
        "source id {source_id} not in graph"
    );

    let mut out_ports: Vec<u32> = g
        .ports
        .iter()
        .filter(|(_, p)| p.node_id == source_id && p.direction == PortDirection::Output)
        .map(|(id, _)| *id)
        .collect();
    out_ports.sort();
    anyhow::ensure!(
        !out_ports.is_empty(),
        "source {source_id} has no output ports yet — try again in a moment"
    );
    let mut in_ports = g.sink_input_port_ids(sink_id);
    in_ports.sort();
    anyhow::ensure!(
        !in_ports.is_empty(),
        "sink {sink_desc} has no input ports"
    );

    drop(g);

    let pairs: Vec<(u32, u32)> = match (out_ports.len(), in_ports.len()) {
        (1, _) => in_ports.iter().map(|&i| (out_ports[0], i)).collect(),
        (_, 1) => out_ports.iter().map(|&o| (o, in_ports[0])).collect(),
        (n, m) if n == m => out_ports.iter().copied().zip(in_ports.iter().copied()).collect(),
        (n, m) => {
            warn!(
                out_ports = n,
                in_ports = m,
                sink = %sink_desc,
                "port count mismatch; falling back to full mesh",
            );
            out_ports
                .iter()
                .flat_map(|&o| in_ports.iter().map(move |&i| (o, i)))
                .collect()
        }
    };

    let mut created: Vec<pipewire::link::Link> = Vec::new();
    for (out_port, in_port) in pairs {
        let props = properties! {
            "link.output.node" => source_id.to_string(),
            "link.output.port" => out_port.to_string(),
            "link.input.node"  => sink_id.to_string(),
            "link.input.port"  => in_port.to_string(),
            "object.linger" => "false",
        };
        let link: pipewire::link::Link = core
            .create_object("link-factory", &props)
            .with_context(|| {
                format!("create link {source_id}:{out_port} -> {sink_id}:{in_port}")
            })?;
        created.push(link);
    }

    Ok((sink_desc, created))
}

/// Convenience: link our own source node to `sink_id`. Used by the monitor
/// and auto-patch paths that always originate from our virtual source.
fn create_links_to_sink(
    core: &Core,
    graph: &Rc<RefCell<Graph>>,
    sink_id: u32,
) -> anyhow::Result<(String, Vec<pipewire::link::Link>)> {
    let own = graph
        .borrow()
        .own_node_id
        .ok_or_else(|| anyhow::anyhow!("own source node not yet registered"))?;
    create_links(core, graph, own, sink_id)
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

enum SourceKind {
    /// `Stream/Output/Audio` — routed via WP `target.object` metadata.
    Stream,
    /// `Audio/Source` — routed via explicit pipewire link objects, since
    /// WP's routing policy doesn't apply to hardware capture nodes.
    Device,
}

fn source_kind(graph: &Rc<RefCell<Graph>>, source_id: u32) -> anyhow::Result<SourceKind> {
    let g = graph.borrow();
    let node = g
        .nodes
        .get(&source_id)
        .ok_or_else(|| anyhow::anyhow!("source id {source_id} not in graph"))?;
    match node.media_class.as_str() {
        "Stream/Output/Audio" => Ok(SourceKind::Stream),
        "Audio/Source" => Ok(SourceKind::Device),
        other => Err(anyhow::anyhow!(
            "source id {source_id} has unsupported media.class `{other}`"
        )),
    }
}

/// Route a hardware `Audio/Source` (USB mic, line-in, ...) into one of
/// our companion sinks by creating pipewire link objects between the
/// device's capture output ports and the sink's playback input ports.
///
/// Additive by design: the device continues to feed anything it was
/// already connected to (a monitoring app, another consumer). "Off" just
/// removes these links; it does not touch the device's other routings.
///
/// Re-routing (Music → Vox or vice versa) drops the previous link set
/// before creating the new one — otherwise the device would feed *both*
/// strips simultaneously, which is never what the user asked for.
fn link_device_source(
    core: &Core,
    graph: &Rc<RefCell<Graph>>,
    source_id: u32,
    target: SinkRole,
) -> anyhow::Result<()> {
    let sink_id = {
        let g = graph.borrow();
        let sink_name = match target {
            SinkRole::Music => g.music_sink_name.as_str(),
            SinkRole::Vox => g.vox_sink_name.as_str(),
        };
        g.nodes
            .iter()
            .find(|(_, n)| n.media_class == "Audio/Sink" && n.name == sink_name)
            .map(|(id, _)| *id)
            .ok_or_else(|| {
                anyhow::anyhow!("companion sink `{sink_name}` not yet registered")
            })?
    };
    // Drop any prior device link set for this source before creating a
    // new one. Removing the entry drops the Link proxies, which tears
    // the pipewire links (`object.linger=false` on creation).
    graph.borrow_mut().device_links.remove(&source_id);
    let (sink_desc, links) = create_links(core, graph, source_id, sink_id)?;
    graph
        .borrow_mut()
        .device_links
        .insert(source_id, ActiveLinkSet { sink_id, _links: links });
    info!(source_id, sink = %sink_desc, role = ?target, "linked device source");
    Ok(())
}

/// Route an external `Stream/Output/Audio` producer via WirePlumber's
/// `target.object` metadata. Passing `Some(role)` pins the source to our
/// music/vox sink; passing `None` clears the pin, letting WP restore its
/// default routing (typically the default audio sink). WP is responsible
/// for actually creating/destroying the pipewire links — this is a pure
/// policy write, so it correctly moves the routing rather than doubling
/// it up alongside the existing default-sink link (which was the flaw of
/// the previous "just add another link" implementation).
///
/// Value format learned by tracing `pactl move-sink-input`: WP's linking
/// policy only reacts when the metadata property is typed `Spa:Id` with
/// the target's `object.serial` (not `node.name`, not `node.id`). We also
/// write the legacy `target.node` key with the sink's node id — pactl
/// does the same for compat with older WP releases, and it's cheap.
fn route_source_via_metadata(
    graph: &Rc<RefCell<Graph>>,
    source_id: u32,
    target: Option<SinkRole>,
) -> anyhow::Result<()> {
    let g = graph.borrow();
    let metadata = g
        .default_metadata
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("`default` metadata not yet available from pipewire"))?;
    anyhow::ensure!(
        g.nodes.contains_key(&source_id),
        "source id {source_id} not in graph"
    );
    match target {
        Some(role) => {
            let sink_name = match role {
                SinkRole::Music => g.music_sink_name.as_str(),
                SinkRole::Vox => g.vox_sink_name.as_str(),
            };
            let (sink_node_id, sink_serial) = g
                .nodes
                .iter()
                .find(|(_, n)| n.media_class == "Audio/Sink" && n.name == sink_name)
                .and_then(|(id, n)| n.object_serial.map(|s| (*id, s)))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "companion sink `{sink_name}` not yet registered (or missing object.serial)"
                    )
                })?;
            let sink_name = sink_name.to_string();
            metadata.set_property(
                source_id,
                "target.object",
                Some("Spa:Id"),
                Some(&sink_serial.to_string()),
            );
            metadata.set_property(
                source_id,
                "target.node",
                Some("Spa:Id"),
                Some(&sink_node_id.to_string()),
            );
            info!(source_id, sink = %sink_name, sink_serial, role = ?role, "routed source via metadata");
        }
        None => {
            // Clearing (value=None) tells WP: no override, fall back to
            // default sink policy. Both keys are deleted so any stale
            // legacy `target.node` entry doesn't override the fallback.
            metadata.set_property(source_id, "target.object", None, None);
            metadata.set_property(source_id, "target.node", None, None);
            info!(source_id, "cleared source routing (restored to default)");
        }
    }
    Ok(())
}

// -----------------------------------------------------------------------------
// PipeWire main-thread entrypoint
// -----------------------------------------------------------------------------

fn run(
    cfg: Config,
    consumer: Consumer<[f32; 2]>,
    stop: Arc<AtomicBool>,
    cmd_rx: pw_channel::Receiver<Command>,
) -> Result<()> {
    pw::init();

    let mainloop = MainLoop::new(None).context("MainLoop::new")?;
    let context = Context::new(&mainloop).context("Context::connect")?;
    let core = context.connect(None).context("Context::connect")?;

    let graph: Rc<RefCell<Graph>> = Rc::new(RefCell::new(Graph {
        auto_patch_target: cfg.auto_patch_target.clone(),
        music_sink_name: format!("{}-{}", cfg.node_name, SinkRole::Music.suffix()),
        vox_sink_name: format!("{}-{}", cfg.node_name, SinkRole::Vox.suffix()),
        ..Graph::default()
    }));

    // Passthrough rings, one per companion sink. Sized for ~half a second at
    // the target rate: large enough to absorb sink/source callback tick jitter
    // without piling up perceivable latency in the mic feed. Both rings carry
    // stereo pairs — the sink callback pushes each incoming frame verbatim
    // (no downmix), and the source callback applies per-strip pan
    // (constant-power law, per-channel) so the stereo image survives from
    // whatever's feeding the sink all the way to the mic feed.
    let input_ring_frames = (cfg.sample_rate as usize / 2).max(1024);
    let (music_producer, music_consumer) = RingBuffer::<[f32; 2]>::new(input_ring_frames);
    let (vox_producer, vox_consumer) = RingBuffer::<[f32; 2]>::new(input_ring_frames);

    // Registry listener. Wrap the registry in an Rc so both the global
    // callback (which needs to `bind` new node proxies) and the run-scope
    // hold on it. The registry itself must outlive `_registry_listener`,
    // so keep the Rc alive for the whole main-loop scope.
    let registry: Rc<Registry> = Rc::new(core.get_registry().context("get_registry")?);
    let graph_g = graph.clone();
    let graph_r = graph.clone();
    let registry_for_globals = registry.clone();
    let _registry_listener = registry
        .add_listener_local()
        .global(move |obj| on_global(&graph_g, &registry_for_globals, obj))
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
        /// Stereo TTS drain — the synthesis pipeline's output. TTS is
        /// currently mono-native and duplicated to L=R at the ring
        /// boundary (see `tts::push_samples_backpressured`); future
        /// stereo TTS or FX in the chain can drop straight in.
        tts_consumer: Consumer<[f32; 2]>,
        /// Stereo music-sink drain. Empty when nobody's routed into the
        /// `-music` node; pops return Err and contribute 0 to the sum.
        music_consumer: Consumer<[f32; 2]>,
        /// Stereo vox-sink drain. Same semantics as music — the sink
        /// callback pushes each incoming stereo frame verbatim.
        vox_consumer: Consumer<[f32; 2]>,
        /// Shared mixer atomics. Read per-frame; updates from HTTP are
        /// picked up on the very next process cycle.
        mixer: Arc<AtomicMixer>,
        /// Browser-monitor PCM tap. Fired after each process() with the
        /// final interleaved stereo mic feed (L, R, L, R, ...), so
        /// subscribers hear exactly what downstream sinks hear — including
        /// pan and per-strip gain.
        monitor_tap: broadcast::Sender<Arc<[f32]>>,
        /// Frames where every source contributed 0. Kept as a diagnostic.
        silent_frames: u64,
    }
    let state = StreamState {
        tts_consumer: consumer,
        music_consumer,
        vox_consumer,
        mixer: cfg.mixer.clone(),
        monitor_tap: cfg.monitor_tap.clone(),
        silent_frames: 0,
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
            // Interleaved stereo: each frame is 2 × f32.
            let bytes_per_frame = SOURCE_CHANNELS as usize * std::mem::size_of::<f32>();

            let capacity_frames = if let Some(slice) = data.data() {
                let out: &mut [f32] = bytemuck_cast_slice_mut(slice);
                let frames = out.len() / SOURCE_CHANNELS as usize;

                // Snapshot the mixer state once per process() rather than
                // once per frame. Atomics are cheap but this is still fewer
                // loads and the mixer never changes mid-buffer meaningfully.
                let (tts_l, tts_r) = state.mixer.tts.stereo_gains();
                let (music_l, music_r) = state.mixer.music.stereo_gains();
                let (vox_l, vox_r) = state.mixer.vox.stereo_gains();
                let (master_gain, master_muted) = state.mixer.master();
                // Vox is a capture source for STT / recording — its samples
                // reach that path via the sink's broadcast tap regardless of
                // this flag. Only its contribution to the mic-feed sum is
                // gated. Metering (below) still reflects the strip's panned
                // signal so users can see input level even when the strip
                // isn't routed out.
                let vox_out = state.mixer.vox_to_output();

                // Buffer the final stereo mic feed so we can broadcast it to
                // the browser monitor after we've filled the buffer.
                // Interleaved [L, R, L, R, ...] so pan is preserved for the
                // Opus stereo encoder on the other side of the tap. Only
                // allocate when at least one subscriber is attached.
                let want_tap = state.monitor_tap.receiver_count() > 0;
                let mut tap_buf: Vec<f32> = if want_tap {
                    Vec::with_capacity(frames * SOURCE_CHANNELS as usize)
                } else {
                    Vec::new()
                };

                // Per-strip peak magnitudes (pre-master) accumulated across
                // this whole buffer. We update the mixer's atomics once at
                // the bottom rather than per-frame so the RT hot path only
                // touches non-atomic locals inside the loop.
                let mut tts_peak = 0.0f32;
                let mut music_peak = 0.0f32;
                let mut vox_peak = 0.0f32;
                let mut master_l_peak = 0.0f32;
                let mut master_r_peak = 0.0f32;

                let mut any_audio = false;
                for i in 0..frames {
                    // Always drain each ring even if muted — otherwise the
                    // upstream sink callback will spin against a full ring
                    // and drop input frames instead of just being silenced.
                    // Rings carry stereo `[L, R]` pairs; underrun contributes
                    // silence to both channels.
                    let [tts_lin, tts_rin] = state.tts_consumer.pop().unwrap_or([0.0, 0.0]);
                    let [mus_lin, mus_rin] = state.music_consumer.pop().unwrap_or([0.0, 0.0]);
                    let [vox_lin, vox_rin] = state.vox_consumer.pop().unwrap_or([0.0, 0.0]);

                    // Per-strip contributions to the mix bus (pre-master).
                    // Constant-power pan is applied per-channel: the L input
                    // is attenuated by `l_gain` (= cos(angle)) into the L bus,
                    // R by `r_gain` (= sin(angle)) into the R bus. At pan=0
                    // both channels see 0.707 → stereo image preserved (with
                    // the classic ~3dB center dip). At pan=±1 the opposite
                    // input's channel is muted. For a mono-duplicated source
                    // (TTS today) this is identical to the previous mono-in,
                    // pan-fanned-to-stereo behavior.
                    let tts_l_c = tts_lin * tts_l;
                    let tts_r_c = tts_rin * tts_r;
                    let mus_l_c = mus_lin * music_l;
                    let mus_r_c = mus_rin * music_r;
                    let vox_l_c = vox_lin * vox_l;
                    let vox_r_c = vox_rin * vox_r;

                    // Meter magnitude per strip = max(|L|, |R|) of the
                    // panned contribution. Matches what a channel VU on a
                    // physical mixer shows post-fader/pre-master.
                    tts_peak = tts_peak.max(tts_l_c.abs().max(tts_r_c.abs()));
                    music_peak = music_peak.max(mus_l_c.abs().max(mus_r_c.abs()));
                    vox_peak = vox_peak.max(vox_l_c.abs().max(vox_r_c.abs()));

                    let (l, r) = if master_muted {
                        (0.0, 0.0)
                    } else {
                        let (vox_out_l, vox_out_r) = if vox_out {
                            (vox_l_c, vox_r_c)
                        } else {
                            (0.0, 0.0)
                        };
                        let l = (tts_l_c + mus_l_c + vox_out_l) * master_gain;
                        let r = (tts_r_c + mus_r_c + vox_out_r) * master_gain;
                        (l.clamp(-1.0, 1.0), r.clamp(-1.0, 1.0))
                    };
                    master_l_peak = master_l_peak.max(l.abs());
                    master_r_peak = master_r_peak.max(r.abs());

                    if l != 0.0 || r != 0.0 {
                        any_audio = true;
                    }
                    let base = i * SOURCE_CHANNELS as usize;
                    out[base] = l;
                    out[base + 1] = r;
                    if want_tap {
                        tap_buf.push(l);
                        tap_buf.push(r);
                    }
                }

                // Publish the per-buffer peaks to the mixer atomics for
                // `/mixer/levels`. Cheap: three CAS-loop fetch_max ops plus
                // two for master, once per cycle rather than per-frame.
                state.mixer.tts.observe_peak(tts_peak);
                state.mixer.music.observe_peak(music_peak);
                state.mixer.vox.observe_peak(vox_peak);
                state.mixer.observe_master_peak(master_l_peak, master_r_peak);

                if want_tap && !tap_buf.is_empty() {
                    let _ = state.monitor_tap.send(Arc::from(tap_buf));
                }
                if !any_audio {
                    state.silent_frames = state.silent_frames.saturating_add(frames as u64);
                    if state.silent_frames.is_power_of_two() {
                        debug!(silent_frames = state.silent_frames, "source ticks with no audio");
                    }
                }
                frames
            } else {
                0
            };

            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = bytes_per_frame as _;
            *chunk.size_mut() = (bytes_per_frame * capacity_frames) as _;
        })
        .register()
        .context("Stream::register")?;

    // Format param.
    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(cfg.sample_rate);
    info.set_channels(SOURCE_CHANNELS);
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

    // Two companion sink nodes, each with a fixed routing purpose:
    //   * `-music` → downmix + push into music ring (mixed via the mixer's
    //                music strip in the source callback)
    //   * `-vox`   → downmix + push into vox ring (mixed via the mixer's
    //                vox strip) AND publish stereo to the broadcast tap for
    //                downstream FX / recording consumers
    // Both streams are kept alive for the pw thread lifetime; dropping
    // either tears its node down.
    let (_music_stream, _music_listener) = register_sink_stream(
        &core,
        &cfg,
        SinkRole::Music,
        music_producer,
        None,
    )
    .context("register music sink stream")?;
    let (_vox_stream, _vox_listener) = register_sink_stream(
        &core,
        &cfg,
        SinkRole::Vox,
        vox_producer,
        Some(cfg.input_tap.clone()),
    )
    .context("register vox sink stream")?;

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

/// Register one of the companion `Audio/Sink` nodes. Called twice from
/// [`run`], once per [`SinkRole`]. Returns the [`Stream`] + its listener so
/// the caller can bind them to a scope that lives as long as the pw main
/// loop — dropping either tears the node down.
///
/// Both roles push each incoming stereo frame verbatim into their
/// dedicated SPSC ring; the source callback drains the ring and applies
/// per-strip pan/gain/mute via the shared mixer state. The Vox sink
/// additionally publishes the raw stereo interleaved samples on the
/// broadcast tap for downstream FX / recording consumers.
fn register_sink_stream(
    core: &Core,
    cfg: &Config,
    role: SinkRole,
    input_producer: Producer<[f32; 2]>,
    input_tap: Option<broadcast::Sender<Arc<[f32]>>>,
) -> Result<(Stream, pw::stream::StreamListener<SinkState>)> {
    let sink_node_name = format!("{}-{}", cfg.node_name, role.suffix());
    let sink_node_description = format!("{} {}", cfg.node_description, role.description());

    let props = properties! {
        *keys::MEDIA_TYPE => "Audio",
        *keys::MEDIA_CATEGORY => "Capture",
        *keys::MEDIA_ROLE => role.media_role(),
        *keys::MEDIA_CLASS => "Audio/Sink",
        *keys::NODE_NAME => sink_node_name.as_str(),
        *keys::NODE_DESCRIPTION => sink_node_description.as_str(),
        "node.virtual" => "true",
    };
    let stream = Stream::new(core, &sink_node_name, props).context("input Stream::new")?;

    // Vox is the only role that needs a broadcast tap; enforce here so a
    // future refactor can't silently drop the FX/recording feed.
    if matches!(role, SinkRole::Vox) {
        anyhow::ensure!(input_tap.is_some(), "Vox sink requires an input_tap");
    }

    let state = SinkState {
        role,
        producer: input_producer,
        tap: input_tap,
        frames_seen: 0,
        overflow_frames: 0,
    };

    let listener = stream
        .add_local_listener_with_user_data::<SinkState>(state)
        .state_changed(move |_, _, old, new| {
            info!(?old, ?new, role = ?role, "PipeWire input stream state changed");
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
            // For capture streams, pipewire populates the buffer BEFORE the
            // callback and reports the valid range via chunk metadata. The
            // raw slice from `data()` is the full allocation, which may
            // contain garbage or stale samples past `offset + size` — reading
            // that as audio is what caused the "rhythmic clipping" bug.
            let chunk = data.chunk();
            let offset = chunk.offset() as usize;
            let size = chunk.size() as usize;
            let stride = chunk.stride() as usize;
            let bytes_per_frame = SINK_CHANNELS as usize * std::mem::size_of::<f32>();
            // Stride should equal bytes-per-frame for interleaved F32LE; if
            // pipewire ever hands us something different we bail rather than
            // guess.
            if stride != bytes_per_frame {
                return;
            }
            let Some(slice) = data.data() else {
                return;
            };
            let end = offset.saturating_add(size);
            if end > slice.len() || size == 0 {
                return;
            }
            let valid = &slice[offset..end];
            let frames = size / bytes_per_frame;
            if frames == 0 {
                return;
            }
            // SAFETY: pipewire negotiated F32LE with SINK_CHANNELS channels;
            // the buffer is aligned + sized appropriately.
            let stereo: &[f32] = bytemuck_cast_slice(valid);

            // Push each incoming stereo frame verbatim so the source
            // callback can apply per-strip pan/gain/mute without ever
            // collapsing to mono. Vox also mirrors the raw stereo frame
            // to the broadcast tap for downstream FX / recording consumers.
            let mut dropped = 0usize;
            for f in 0..frames {
                let base = f * SINK_CHANNELS as usize;
                let pair = [stereo[base], stereo[base + 1]];
                if state.producer.push(pair).is_err() {
                    dropped = frames - f;
                    break;
                }
            }
            if dropped > 0 {
                state.overflow_frames =
                    state.overflow_frames.saturating_add(dropped as u64);
                if state.overflow_frames.is_power_of_two() {
                    debug!(
                        role = ?state.role,
                        dropped = state.overflow_frames,
                        "sink ring overflow (source callback behind?)"
                    );
                }
            }
            if let Some(tap) = state.tap.as_ref() {
                if tap.receiver_count() > 0 {
                    let _ = tap.send(Arc::from(stereo));
                }
            }

            state.frames_seen = state.frames_seen.saturating_add(frames as u64);
            if state.frames_seen > 0 && state.frames_seen.is_power_of_two() {
                debug!(role = ?state.role, frames = state.frames_seen, "input frames processed");
            }
        })
        .register()
        .context("input Stream::register")?;

    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(cfg.sample_rate);
    info.set_channels(SINK_CHANNELS);
    let obj = Object {
        type_: libspa::sys::SPA_TYPE_OBJECT_Format,
        id: libspa::sys::SPA_PARAM_EnumFormat,
        properties: info.into(),
    };
    let values: Vec<u8> =
        PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj))
            .context("serialize input format pod")?
            .0
            .into_inner();
    let mut params = [Pod::from_bytes(&values).context("input format pod parse")?];

    stream
        .connect(
            libspa::utils::Direction::Input,
            None,
            StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
            &mut params,
        )
        .context("input Stream::connect")?;

    info!(node = %sink_node_name, "PipeWire input sink node started");
    Ok((stream, listener))
}

struct SinkState {
    role: SinkRole,
    /// Mono ring producer drained by the source callback (populated for
    /// every role — both music and vox now mix through the source).
    producer: Producer<[f32; 2]>,
    /// Broadcast sender for downstream FX / recording consumers. Only the
    /// Vox sink attaches one.
    tap: Option<broadcast::Sender<Arc<[f32]>>>,
    frames_seen: u64,
    overflow_frames: u64,
}

/// Reinterpret a byte slice as an f32 slice. PipeWire aligns buffers for the
/// negotiated F32LE sample format.
fn bytemuck_cast_slice_mut(bytes: &mut [u8]) -> &mut [f32] {
    let len = bytes.len() / std::mem::size_of::<f32>();
    let ptr = bytes.as_mut_ptr() as *mut f32;
    unsafe { std::slice::from_raw_parts_mut(ptr, len) }
}

fn bytemuck_cast_slice(bytes: &[u8]) -> &[f32] {
    let len = bytes.len() / std::mem::size_of::<f32>();
    let ptr = bytes.as_ptr() as *const f32;
    unsafe { std::slice::from_raw_parts(ptr, len) }
}
