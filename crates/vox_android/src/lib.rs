//! JNI bridge for the Android dictation app (`android/app`): loads
//! Parakeet (passes 2 and 3) and Zipformer (pass 1) from a models
//! directory, runs a [`vox_transcribe::Engine`] fed with microphone
//! samples from Kotlin, and hands its events back as JSON.
//!
//! Every handle is a `Box` pointer passed to Kotlin as a `long`. The
//! Kotlin side (`com.enigmacurry.rpg_vox_scribe.Native`) owns the order of
//! calls: no `push`/`poll` once `finish` has been called, no `start`
//! after `freeModels`.

use std::collections::HashMap;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Once};
use std::time::Duration;

use anyhow::{Context as _, Result};
use crossbeam_channel::Receiver;
use jni::objects::{JClass, JFloatArray, JString};
use jni::sys::{jfloat, jint, jlong, jstring};
use jni::JNIEnv;
use serde_json::{json, Value};
use vox_transcribe::sherpa::{Parakeet, ParakeetConfig, Zipformer, ZipformerConfig};
use vox_transcribe::{
    Change, Engine, EngineConfig, Event, OfflineRecognizer, StreamingRecognizer,
};

/// Same layout as vox_scribe's models dir.
const PARAKEET: &str = "parakeet-tdt-0.6b-v2";
const ZIPFORMER: &str = "streaming-zipformer";

struct Models {
    offline: Arc<dyn OfflineRecognizer>,
    zipformer: Zipformer,
}

struct Session {
    engine: Engine,
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

/// Compressed bytes read by the running [`unpack`], for progress.
static UNPACKED: AtomicU64 = AtomicU64::new(0);

struct Counting<R>(R);

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.0.read(buf)?;
        UNPACKED.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

/// Extract the entries named in `spec` (`<path below top dir>\t<name>`
/// per line) from a `.tar.bz2` into `dest`.
fn unpack(archive: &Path, dest: &Path, spec: &str) -> Result<()> {
    UNPACKED.store(0, Ordering::Relaxed);
    let wanted: HashMap<&str, &str> = spec.lines().filter_map(|l| l.split_once('\t')).collect();
    let file = std::fs::File::open(archive).with_context(|| format!("open {}", archive.display()))?;
    let bz = bzip2::read::MultiBzDecoder::new(BufReader::with_capacity(1 << 20, Counting(file)));
    let mut tar = tar::Archive::new(bz);
    for entry in tar.entries().context("read archive")? {
        let mut entry = entry.context("read archive entry")?;
        let path = entry.path()?.to_string_lossy().into_owned();
        // "<stem>/encoder.int8.onnx" -> "encoder.int8.onnx"
        let name = path.split_once('/').map_or(path.as_str(), |(_, rest)| rest);
        if let Some(to) = wanted.get(name) {
            entry
                .unpack(dest.join(to))
                .with_context(|| format!("extract {name}"))?;
        }
    }
    Ok(())
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

fn throw(env: &mut JNIEnv, err: &anyhow::Error) {
    let _ = env.throw_new("java/lang/RuntimeException", format!("{err:#}"));
}

fn new_string(env: &mut JNIEnv, s: &str) -> jstring {
    env.new_string(s)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// Routes `tracing` to logcat under the tag `vox`.
fn init_logging() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_writer(|| Logcat(Vec::new()))
            .with_ansi(false)
            .without_time()
            .with_env_filter("info")
            .try_init();
    });
}

struct Logcat(Vec<u8>);

impl std::io::Write for Logcat {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for Logcat {
    fn drop(&mut self) {
        let line = String::from_utf8_lossy(&self.0);
        let line = line.trim_end();
        if line.is_empty() {
            return;
        }
        if let Ok(msg) = std::ffi::CString::new(line.replace('\0', "")) {
            // ANDROID_LOG_INFO
            unsafe { __android_log_write(4, c"vox".as_ptr(), msg.as_ptr()) };
        }
    }
}

#[cfg(target_os = "android")]
#[link(name = "log")]
extern "C" {
    fn __android_log_write(
        prio: i32,
        tag: *const std::ffi::c_char,
        text: *const std::ffi::c_char,
    ) -> i32;
}

/// Off Android (e.g. `cargo check` on the host) logs go to stderr.
#[cfg(not(target_os = "android"))]
unsafe fn __android_log_write(
    _prio: i32,
    _tag: *const std::ffi::c_char,
    text: *const std::ffi::c_char,
) -> i32 {
    eprintln!("{}", std::ffi::CStr::from_ptr(text).to_string_lossy());
    0
}

/// `unpack(archive, destDir, spec)`: blocks until done; throws on failure.
#[no_mangle]
pub extern "system" fn Java_com_enigmacurry_rpg_1vox_1scribe_Native_unpack(
    mut env: JNIEnv,
    _class: JClass,
    archive: JString,
    dest: JString,
    spec: JString,
) {
    init_logging();
    let args: Result<(String, String, String)> = (|| {
        Ok((
            env.get_string(&archive)?.into(),
            env.get_string(&dest)?.into(),
            env.get_string(&spec)?.into(),
        ))
    })();
    let result = args.and_then(|(a, d, s)| unpack(Path::new(&a), Path::new(&d), &s));
    if let Err(e) = result {
        throw(&mut env, &e);
    }
}

/// Compressed bytes the running `unpack` has read so far.
#[no_mangle]
pub extern "system" fn Java_com_enigmacurry_rpg_1vox_1scribe_Native_unpackProgress(
    _env: JNIEnv,
    _class: JClass,
) -> jlong {
    UNPACKED.load(Ordering::Relaxed) as jlong
}

/// `load(modelsDir, threads)`: a models handle, or throws.
#[no_mangle]
pub extern "system" fn Java_com_enigmacurry_rpg_1vox_1scribe_Native_load(
    mut env: JNIEnv,
    _class: JClass,
    dir: JString,
    threads: jint,
) -> jlong {
    init_logging();
    let dir: String = match env.get_string(&dir) {
        Ok(s) => s.into(),
        Err(e) => {
            throw(&mut env, &anyhow::anyhow!("models dir: {e}"));
            return 0;
        }
    };
    match load(Path::new(&dir), threads) {
        Ok(m) => Box::into_raw(Box::new(m)) as jlong,
        Err(e) => {
            throw(&mut env, &e);
            0
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_com_enigmacurry_rpg_1vox_1scribe_Native_freeModels(
    _env: JNIEnv,
    _class: JClass,
    models: jlong,
) {
    if models != 0 {
        drop(unsafe { Box::from_raw(models as *mut Models) });
    }
}

/// `start(models, sampleRate)`: a session handle for one recording.
#[no_mangle]
pub extern "system" fn Java_com_enigmacurry_rpg_1vox_1scribe_Native_start(
    _env: JNIEnv,
    _class: JClass,
    models: jlong,
    rate: jint,
) -> jlong {
    let models = unsafe { &*(models as *const Models) };
    let streaming = Box::new(models.zipformer.session()) as Box<dyn StreamingRecognizer>;
    let engine = Engine::spawn(
        EngineConfig::new(rate as u32),
        Some(streaming),
        models.offline.clone(),
    );
    let events = engine.events().clone();
    Box::into_raw(Box::new(Session { engine, events })) as jlong
}

#[no_mangle]
pub extern "system" fn Java_com_enigmacurry_rpg_1vox_1scribe_Native_push(
    env: JNIEnv,
    _class: JClass,
    session: jlong,
    samples: JFloatArray,
    len: jint,
) {
    let session = unsafe { &*(session as *const Session) };
    let mut buf = vec![0 as jfloat; len.max(0) as usize];
    if env.get_float_array_region(&samples, 0, &mut buf).is_ok() {
        session.engine.push(&buf);
    }
}

#[no_mangle]
pub extern "system" fn Java_com_enigmacurry_rpg_1vox_1scribe_Native_breakParagraph(
    _env: JNIEnv,
    _class: JClass,
    session: jlong,
) {
    let session = unsafe { &*(session as *const Session) };
    session.engine.break_paragraph();
}

/// `poll(session, timeoutMs)`: a JSON array of events, possibly empty.
#[no_mangle]
pub extern "system" fn Java_com_enigmacurry_rpg_1vox_1scribe_Native_poll(
    mut env: JNIEnv,
    _class: JClass,
    session: jlong,
    timeout_ms: jint,
) -> jstring {
    let session = unsafe { &*(session as *const Session) };
    let events = drain(
        &session.events,
        Duration::from_millis(timeout_ms.max(0) as u64),
    );
    new_string(&mut env, &events.to_string())
}

/// `finish(session)`: drains every pass and returns the final transcript
/// as JSON (`{"paragraphs": [...]}`). Frees the session.
#[no_mangle]
pub extern "system" fn Java_com_enigmacurry_rpg_1vox_1scribe_Native_finish(
    mut env: JNIEnv,
    _class: JClass,
    session: jlong,
) -> jstring {
    let session = unsafe { Box::from_raw(session as *mut Session) };
    let transcript = session.engine.finish();
    let json = serde_json::to_string(&transcript).unwrap_or_else(|_| "{}".into());
    new_string(&mut env, &json)
}
