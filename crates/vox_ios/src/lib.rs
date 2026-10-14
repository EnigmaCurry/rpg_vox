//! C ABI for the iOS dictation app (`ios/`): loads Parakeet (passes 2
//! and 3) and Zipformer (pass 1) from a models directory, runs a
//! [`vox_transcribe::Engine`] fed with microphone samples from Swift, and
//! hands its events back as JSON. The declarations Swift sees are in
//! `ios/Scribe/vox_ios.h`.
//!
//! Every handle is a `Box` pointer. The Swift side owns the order of
//! calls: nothing on a session after `vox_free`, no `vox_start` after
//! `vox_free_models`. `vox_push` and `vox_poll` may run alongside
//! `vox_finish`. Strings returned to Swift are freed with
//! `vox_string_free`.

use std::ffi::{c_char, CStr, CString};
use std::path::Path;
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use anyhow::{Context as _, Result};
use crossbeam_channel::Receiver;
use serde_json::{json, Value};
use vox_transcribe::sherpa::{Parakeet, ParakeetConfig, Zipformer, ZipformerConfig};
use vox_transcribe::{
    Change, Engine, EngineConfig, Event, OfflineRecognizer, Pusher, StreamingRecognizer,
};

/// Same layout as vox_scribe's models dir.
const PARAKEET: &str = "parakeet-tdt-0.6b-v2";
const ZIPFORMER: &str = "streaming-zipformer";

pub struct Models {
    offline: Arc<dyn OfflineRecognizer>,
    zipformer: Zipformer,
}

pub struct Session {
    /// Taken by `vox_finish`; pushes after that are dropped.
    engine: Mutex<Option<Engine>>,
    pusher: Pusher,
    events: Receiver<Event>,
}

fn load(dir: &Path, threads: i32) -> Result<Models> {
    let mut pc = ParakeetConfig::from_dir(&dir.join(PARAKEET));
    pc.num_threads = threads;
    let offline = Arc::new(Parakeet::open(&pc).context("loading Parakeet")?);
    let mut zc = ZipformerConfig::from_dir(&dir.join(ZIPFORMER));
    zc.num_threads = threads;
    let zipformer = Zipformer::open(&zc).context("loading Zipformer")?;
    Ok(Models { offline, zipformer })
}

fn change_name(c: Change) -> &'static str {
    match c {
        Change::Opened => "opened",
        Change::Partial => "partial",
        Change::Final => "final",
        Change::Revised => "revised",
        Change::ClipRemoved => "clip_removed",
        Change::Closed => "closed",
        Change::Corrected => "corrected",
        Change::Hardened => "hardened",
    }
}

/// Everything queued, waiting up to `timeout` for the first event. Level
/// events are collapsed to the latest one.
fn drain(events: &Receiver<Event>, timeout: Duration) -> Value {
    let mut out = Vec::new();
    let mut level = None;
    let first = events.recv_timeout(timeout).ok();
    for ev in first.into_iter().chain(events.try_iter()) {
        match ev {
            Event::Paragraph { paragraph, change } => out.push(json!({
                "t": "paragraph",
                "change": change_name(change),
                "p": paragraph,
            })),
            Event::ParagraphRemoved { id } => out.push(json!({ "t": "removed", "id": id })),
            Event::Level { rms, speaking, .. } => {
                level = Some(json!({ "t": "level", "rms": rms, "speaking": speaking }))
            }
        }
    }
    out.extend(level);
    Value::Array(out)
}

/// Routes `tracing` to stderr, which Xcode and `devicectl --console` show.
fn init_logging() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .without_time()
            .with_env_filter("info")
            .try_init();
    });
}

fn c_string(s: &str) -> *mut c_char {
    CString::new(s.replace('\0', ""))
        .map(CString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

/// Loads the models from `dir`: a models handle, or null with `*err` set
/// to the reason (free it with `vox_string_free`).
///
/// # Safety
/// `dir` is a NUL-terminated string; `err` is null or writable.
#[no_mangle]
pub unsafe extern "C" fn vox_load(
    dir: *const c_char,
    threads: i32,
    err: *mut *mut c_char,
) -> *mut Models {
    init_logging();
    let dir = CStr::from_ptr(dir).to_string_lossy().into_owned();
    match load(Path::new(&dir), threads) {
        Ok(m) => Box::into_raw(Box::new(m)),
        Err(e) => {
            tracing::error!("{e:#}");
            if !err.is_null() {
                *err = c_string(&format!("{e:#}"));
            }
            std::ptr::null_mut()
        }
    }
}

/// # Safety
/// `models` is null or from `vox_load`, with no session still using it.
#[no_mangle]
pub unsafe extern "C" fn vox_free_models(models: *mut Models) {
    if !models.is_null() {
        drop(Box::from_raw(models));
    }
}

/// A session for one source of `rate` Hz mono samples. Without
/// `streaming` (files) there are no pass-1 partials.
///
/// # Safety
/// `models` is from `vox_load` and outlives the session.
#[no_mangle]
pub unsafe extern "C" fn vox_start(models: *const Models, rate: u32, streaming: bool) -> *mut Session {
    let models = &*models;
    let streaming =
        streaming.then(|| Box::new(models.zipformer.session()) as Box<dyn StreamingRecognizer>);
    let engine = Engine::spawn(EngineConfig::new(rate), streaming, models.offline.clone());
    let events = engine.events().clone();
    let pusher = engine.pusher();
    Box::into_raw(Box::new(Session {
        engine: Mutex::new(Some(engine)),
        pusher,
        events,
    }))
}

/// # Safety
/// `session` is from `vox_start`; `samples` points at `len` floats.
#[no_mangle]
pub unsafe extern "C" fn vox_push(session: *const Session, samples: *const f32, len: usize) {
    if samples.is_null() || len == 0 {
        return;
    }
    (*session).pusher.push(std::slice::from_raw_parts(samples, len));
}

/// # Safety
/// `session` is from `vox_start`.
#[no_mangle]
pub unsafe extern "C" fn vox_break_paragraph(session: *const Session) {
    (*session).pusher.break_paragraph();
}

/// A JSON array of events, possibly empty, waiting up to `timeout_ms`
/// for the first.
///
/// # Safety
/// `session` is from `vox_start`.
#[no_mangle]
pub unsafe extern "C" fn vox_poll(session: *const Session, timeout_ms: i32) -> *mut c_char {
    let events = drain(
        &(*session).events,
        Duration::from_millis(timeout_ms.max(0) as u64),
    );
    c_string(&events.to_string())
}

/// Drains every pass and returns the final transcript as JSON
/// (`{"paragraphs": [...]}`). Events keep arriving on `vox_poll`
/// meanwhile. The session stays allocated until `vox_free`.
///
/// # Safety
/// `session` is from `vox_start`.
#[no_mangle]
pub unsafe extern "C" fn vox_finish(session: *const Session) -> *mut c_char {
    let engine = (*session).engine.lock().unwrap_or_else(|e| e.into_inner()).take();
    let transcript = engine.map(Engine::finish).unwrap_or_default();
    c_string(&serde_json::to_string(&transcript).unwrap_or_else(|_| "{}".into()))
}

/// # Safety
/// `session` is null or from `vox_start`, and not used afterwards.
#[no_mangle]
pub unsafe extern "C" fn vox_free(session: *mut Session) {
    if !session.is_null() {
        drop(Box::from_raw(session));
    }
}

/// # Safety
/// `s` is null or a string this library returned.
#[no_mangle]
pub unsafe extern "C" fn vox_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}
