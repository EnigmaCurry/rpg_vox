//! Lightweight per-render profiler for the TTS pipeline.
//!
//! Wired as a `tracing_subscriber::Layer` so it composes with the existing
//! `fmt` subscriber without touching the rest of the code — instrumentation
//! is a `#[tracing::instrument]` attribute or a `let _s = perf::span!("...")`
//! guard on the hot functions, nothing more.
//!
//! Model:
//! * A "render" (top-level) span is any span named [`RENDER_SPAN_NAME`]. The
//!   layer starts collecting descendants when it sees one open.
//! * Every span under a render is timed against Instant::now(); durations
//!   below microsecond resolution round to zero.
//! * When the render root closes, we assemble a [`SpanNode`] tree and push
//!   a [`RenderRecord`] into a bounded VecDeque (older records drop out).
//! * The captured data is dumped on demand via [`snapshot`] /
//!   [`to_chrome_json`] / [`to_text`] — the HTTP layer serves those.
//!
//! Cross-thread safety: we hang state off `tracing_subscriber`'s Registry
//! span extensions (keyed by span::Id). Registry state is a global map so
//! tokio moving a task between worker threads mid-await doesn't split the
//! trace — the span ids remain stable and we look up parent state through
//! the shared registry, not thread-locals.
//!
//! Cost when disabled: literally none. When the layer isn't installed,
//! `#[tracing::instrument]` compiles down to a Span::new + Span::enter that
//! nobody watches, and the whole subscriber machinery no-ops.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tracing::span::{Attributes, Id};
use tracing::{Subscriber, field};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

/// Span name that marks the top of a render tree. Only spans named this
/// (and their descendants) are captured — everything else in the process
/// is ignored by this layer.
pub const RENDER_SPAN_NAME: &str = "render";

/// Cap on how many recent renders we keep in memory. Old entries drop off
/// the front when a new render lands and the ring is full.
pub const MAX_RECENT_RENDERS: usize = 50;

/// Compact per-span node. Times are microseconds relative to the enclosing
/// render root's start; the tree is self-contained (no span ids leak out).
#[derive(Debug, Clone, Serialize)]
pub struct SpanNode {
    pub name: String,
    pub start_us: u64,
    pub dur_us: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<SpanNode>,
}

/// One completed render — what the HTTP layer serves back.
#[derive(Debug, Clone, Serialize)]
pub struct RenderRecord {
    pub id: u64,
    /// Wall-clock start time in ms since UNIX epoch — for sorting / display.
    pub started_at_unix_ms: u64,
    /// Short preview of the input text (empty if no `label` field on the
    /// render span). First ~80 chars of whatever the caller passed.
    pub label: String,
    /// Total render duration in microseconds (matches `root.dur_us`).
    pub total_us: u64,
    /// Number of stereo frames in the mixed audio output. `None` if the
    /// render span didn't populate the `audio_frames` field (older records
    /// or a render path that didn't produce a mix). Divide by
    /// `audio_sample_rate` for seconds of audio.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_frames: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_sample_rate: Option<u32>,
    pub root: SpanNode,
}

impl RenderRecord {
    /// Rendered audio length in microseconds (frames · 1_000_000 / rate).
    /// `None` when either field is missing.
    pub fn audio_us(&self) -> Option<u64> {
        let frames = self.audio_frames?;
        let rate = self.audio_sample_rate? as u64;
        if rate == 0 {
            return None;
        }
        Some(frames.saturating_mul(1_000_000) / rate)
    }

    /// Realtime multiplier: audio_us / total_us. > 1.0 means we generated
    /// audio faster than realtime (good — 10s of audio in 2s of wall-clock
    /// is 5.0×). < 1.0 means the render is slower than realtime.
    pub fn realtime_multiplier(&self) -> Option<f64> {
        let a = self.audio_us()? as f64;
        if self.total_us == 0 {
            return None;
        }
        Some(a / self.total_us as f64)
    }
}

/// Per-span state stashed in the Registry so parent/child assembly works
/// across async awaits (tracing's Registry is a global map, not
/// thread-local).
struct SpanState {
    /// Static name from the span metadata — cheap Copy, no allocation
    /// during the hot path.
    name: &'static str,
    start: Instant,
    /// The render root's start Instant, propagated down the tree so every
    /// SpanNode.start_us can be computed as `own_start - root_start`.
    root_start: Instant,
    /// Immediate parent's id, or None if this IS the render root.
    parent_id: Option<Id>,
    /// True when this span itself opened a render (name == RENDER_SPAN_NAME
    /// and no ancestor was under a render).
    is_root: bool,
    /// On root: label extracted from the span's `label` field.
    label: Option<String>,
    /// On root: populated by [`RingLayer::on_record`] when the caller
    /// calls `Span::current().record("audio_frames", n)` after the mix
    /// is built. Non-root spans ignore this.
    audio_frames: Option<u64>,
    audio_sample_rate: Option<u32>,
    /// Assembled from children as they close, one node per direct child.
    completed_children: Vec<SpanNode>,
}

/// The `tracing_subscriber::Layer` that watches for renders and captures
/// their span trees.
pub struct RingLayer;

impl RingLayer {
    /// Install this layer alongside the fmt subscriber in `main`. Nothing
    /// to configure — the ring buffer + counters are all module-level
    /// singletons.
    pub fn new() -> Self {
        Self
    }
}

impl Default for RingLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Layer<S> for RingLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let name = attrs.metadata().name();

        // Look at the parent's SpanState. If present, this span is under a
        // render and inherits root_start; if absent, this span is only
        // interesting when its own name marks a new render root.
        let parent_state: Option<(Id, Instant)> = span.parent().and_then(|p| {
            p.extensions()
                .get::<SpanState>()
                .map(|s| (p.id().clone(), s.root_start))
        });

        let now = Instant::now();
        let (root_start, parent_id, is_root, label) = match parent_state {
            Some((pid, root_start)) => (root_start, Some(pid), false, None),
            None => {
                if name != RENDER_SPAN_NAME {
                    return;
                }
                let mut v = LabelVisitor::default();
                attrs.record(&mut v);
                (now, None, true, v.label)
            }
        };

        span.extensions_mut().insert(SpanState {
            name,
            start: now,
            root_start,
            parent_id,
            is_root,
            label,
            audio_frames: None,
            audio_sample_rate: None,
            completed_children: Vec::new(),
        });
    }

    fn on_record(&self, id: &Id, values: &tracing::span::Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut exts = span.extensions_mut();
        let Some(state) = exts.get_mut::<SpanState>() else { return };
        if !state.is_root {
            return;
        }
        let mut v = AudioMetaVisitor::default();
        values.record(&mut v);
        if let Some(f) = v.frames {
            state.audio_frames = Some(f);
        }
        if let Some(r) = v.sample_rate {
            state.audio_sample_rate = Some(r);
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let state = {
            let mut exts = span.extensions_mut();
            match exts.remove::<SpanState>() {
                Some(s) => s,
                None => return, // not tracked (not under a render)
            }
        };
        let dur_us = state.start.elapsed().as_micros() as u64;
        let start_us = state.start.saturating_duration_since(state.root_start).as_micros() as u64;

        let node = SpanNode {
            name: state.name.to_string(),
            start_us,
            dur_us,
            children: state.completed_children,
        };

        if state.is_root {
            let record = RenderRecord {
                id: next_render_id(),
                started_at_unix_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0),
                label: state.label.unwrap_or_default(),
                total_us: dur_us,
                audio_frames: state.audio_frames,
                audio_sample_rate: state.audio_sample_rate,
                root: node,
            };
            push_record(record);
        } else if let Some(parent_id) = state.parent_id {
            if let Some(parent) = ctx.span(&parent_id) {
                let mut exts = parent.extensions_mut();
                if let Some(pstate) = exts.get_mut::<SpanState>() {
                    pstate.completed_children.push(node);
                }
            }
        }
    }
}

/// Extracts the `label` field off a render span at creation time. Kept in
/// its own file-private type so [`RingLayer::on_new_span`] doesn't have to
/// carry a closure-shaped Visit impl inline.
#[derive(Default)]
struct LabelVisitor {
    label: Option<String>,
}

impl field::Visit for LabelVisitor {
    fn record_str(&mut self, field: &field::Field, value: &str) {
        if field.name() == "label" {
            self.label = Some(truncate_label(value));
        }
    }
    fn record_debug(&mut self, field: &field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "label" {
            self.label = Some(truncate_label(&format!("{value:?}")));
        }
    }
}

/// Extracts `audio_frames` + `audio_sample_rate` off subsequent
/// `Span::record()` calls on the render root. Split from
/// [`LabelVisitor`] because the fields are declared as `Empty` at span
/// creation and only get recorded later, after the mix is built.
#[derive(Default)]
struct AudioMetaVisitor {
    frames: Option<u64>,
    sample_rate: Option<u32>,
}

impl field::Visit for AudioMetaVisitor {
    fn record_u64(&mut self, field: &field::Field, value: u64) {
        match field.name() {
            "audio_frames" => self.frames = Some(value),
            "audio_sample_rate" => self.sample_rate = Some(value as u32),
            _ => {}
        }
    }
    fn record_i64(&mut self, field: &field::Field, value: i64) {
        if value < 0 {
            return;
        }
        self.record_u64(field, value as u64);
    }
    fn record_debug(&mut self, _field: &field::Field, _value: &dyn std::fmt::Debug) {}
}

fn truncate_label(s: &str) -> String {
    const MAX: usize = 80;
    if s.chars().count() <= MAX {
        return s.to_string();
    }
    let mut out: String = s.chars().take(MAX).collect();
    out.push('…');
    out
}

/// Build the short human-readable label the render root span carries.
/// `[N voices] first ~60 chars of text…` — shows in the record list so the
/// user can tell renders apart at a glance without an ID lookup.
pub fn preview_text(text: &str, config_count: usize) -> String {
    let head: String = text.chars().take(60).collect();
    let ellipsis = if text.chars().count() > 60 { "…" } else { "" };
    format!("[{config_count}v] {head}{ellipsis}")
}

// ---------------- Ring buffer + counters -----------------------------------

fn ring() -> &'static Mutex<VecDeque<RenderRecord>> {
    static RING: OnceLock<Mutex<VecDeque<RenderRecord>>> = OnceLock::new();
    RING.get_or_init(|| Mutex::new(VecDeque::with_capacity(MAX_RECENT_RENDERS)))
}

fn next_render_id() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

fn push_record(record: RenderRecord) {
    let Ok(mut guard) = ring().lock() else {
        return;
    };
    if guard.len() >= MAX_RECENT_RENDERS {
        guard.pop_front();
    }
    guard.push_back(record);
}

/// Snapshot the ring buffer as a Vec (newest last). Cheap clone —
/// [`RenderRecord`] is small trees of Strings and integers.
pub fn snapshot() -> Vec<RenderRecord> {
    ring().lock().map(|g| g.iter().cloned().collect()).unwrap_or_default()
}

/// Look up a single record by id. Returns None if it aged out of the ring.
pub fn get(id: u64) -> Option<RenderRecord> {
    ring()
        .lock()
        .ok()
        .and_then(|g| g.iter().find(|r| r.id == id).cloned())
}

/// Most recently completed render, or None if the ring is empty.
pub fn latest() -> Option<RenderRecord> {
    ring().lock().ok().and_then(|g| g.back().cloned())
}

// ---------------- Output formats -------------------------------------------

/// Serialize a record as chrome://tracing / Perfetto / speedscope JSON.
/// The whole render lives on a single (pid, tid) since our pipeline is
/// serial — flame chart consumers stack children under parents by time,
/// which is exactly the parent/child structure we already captured.
pub fn to_chrome_json(record: &RenderRecord) -> serde_json::Value {
    let mut events: Vec<serde_json::Value> = Vec::new();
    fn walk(node: &SpanNode, events: &mut Vec<serde_json::Value>) {
        events.push(serde_json::json!({
            "name": node.name,
            "ph": "X",
            "ts": node.start_us,
            "dur": node.dur_us,
            "pid": 1,
            "tid": 1,
            "cat": "render",
        }));
        for child in &node.children {
            walk(child, events);
        }
    }
    walk(&record.root, &mut events);
    serde_json::json!({
        "traceEvents": events,
        "displayTimeUnit": "ms",
        "otherData": {
            "id": record.id,
            "label": record.label,
            "started_at_unix_ms": record.started_at_unix_ms,
            "audio_frames": record.audio_frames,
            "audio_sample_rate": record.audio_sample_rate,
            "audio_us": record.audio_us(),
            "realtime_multiplier": record.realtime_multiplier(),
        },
    })
}

/// Render a record as an indented text tree with per-node ms + percentage
/// of the total render time. Cheap to eyeball in a terminal — pipe from
/// `curl /perf/renders/latest.txt`.
pub fn to_text(record: &RenderRecord) -> String {
    let mut out = String::new();
    let label = if record.label.is_empty() {
        String::new()
    } else {
        format!(" — {}", record.label)
    };
    // Header line: total wall-clock plus (when we have it) the audio length
    // and realtime multiplier. `4.20s audio, 3.4× realtime` reads at a
    // glance: "we generated 4.2 seconds of audio in 1.25 seconds of CPU."
    let audio_tag = match (record.audio_us(), record.realtime_multiplier()) {
        (Some(a), Some(m)) => format!(
            " — {:.2}s audio, {:.2}× realtime",
            a as f64 / 1_000_000.0,
            m
        ),
        _ => String::new(),
    };
    let _ = writeln!(
        out,
        "Render #{}{} — {:.2}ms wall-clock{}",
        record.id,
        label,
        record.total_us as f64 / 1000.0,
        audio_tag
    );
    walk_text(&record.root, 0, record.total_us, &mut out);
    out
}

fn walk_text(node: &SpanNode, depth: usize, root_total: u64, out: &mut String) {
    let indent = "  ".repeat(depth);
    let dur_ms = node.dur_us as f64 / 1000.0;
    let pct = if root_total > 0 {
        100.0 * node.dur_us as f64 / root_total as f64
    } else {
        0.0
    };
    let _ = writeln!(
        out,
        "{}├ {:<28} {:>9.2}ms  ({:>5.1}%)",
        indent, node.name, dur_ms, pct
    );
    for child in &node.children {
        walk_text(child, depth + 1, root_total, out);
    }
}
