//! JNI bridge for the Android dictation app (`android/app`): loads
//! Parakeet (passes 2 and 3) and Zipformer (pass 1) from a models
//! directory, runs a [`vox_transcribe::Engine`] fed with microphone
//! samples from Kotlin, and hands its events back as JSON. A session can
//! also keep its audio as Ogg Opus, and [`Player`] reads such a file back
//! for playback.
//!
//! Every handle is a `Box` pointer passed to Kotlin as a `long`. The
//! Kotlin side (`com.enigmacurry.rpg_vox_scribe.Native`) owns the order of
//! calls: nothing on a session after `free`, no `start` after
//! `freeModels`. `push` and `poll` may run alongside `finish`.

use std::collections::HashMap;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use anyhow::{Context as _, Result};
use crossbeam_channel::Receiver;
use jni::objects::{JClass, JFloatArray, JLongArray, JString};
use jni::sys::{jboolean, jfloat, jint, jlong, jstring};
use jni::JNIEnv;
use serde_json::{json, Value};
use vox_audio::opus_file::{Cursor, OpusFile, OpusWriter, RATE as OPUS_RATE};
use vox_transcribe::sherpa::{Parakeet, ParakeetConfig, Zipformer, ZipformerConfig};
use vox_transcribe::{
    Change, Engine, EngineConfig, Event, OfflineRecognizer, Pusher, StreamingRecognizer,
};

/// Same layout as vox_scribe's models dir.
const PARAKEET: &str = "parakeet-tdt-0.6b-v3";

struct Models {
    offline: Arc<dyn OfflineRecognizer>,
    /// Pass 1's model for the language it starts in.
    zipformer: Zipformer,
    /// Its other languages, which pass 1 moves between on its own as the
    /// speaking turns out to be in one of them (`Passes::streaming_also`).
    zipformer_also: Vec<(String, Zipformer)>,
    /// The languages `--translate local` reads, with the one copy of the
    /// models they are read with. `None` when no other language is
    /// chosen, which is translation off -- and always, without the
    /// feature, where nothing then reads it.
    #[cfg_attr(not(feature = "translate-local"), allow(dead_code))]
    translate: Option<Translating>,
}

/// What a session needs to make its own `Interpreter`: the languages,
/// the translator they share, and what tells two of them apart when
/// they are written in the same alphabet.
#[cfg(feature = "translate-local")]
struct Translating {
    native: String,
    others: Vec<String>,
    by: Arc<dyn vox_transcribe::translate::Translate>,
    /// `None` when the classifier is not downloaded, in which case
    /// `check_languages` has already refused any set that would have
    /// needed it -- so this being absent is a set the alphabet can
    /// separate on its own, not a set being guessed at.
    telling: Option<Arc<dyn vox_transcribe::langid::Classify>>,
}

/// Without the feature nothing constructs this; `load` says so rather
/// than translating nothing quietly.
#[cfg(not(feature = "translate-local"))]
struct Translating {}

struct Session {
    /// Taken by `finish`; pushes after that are dropped.
    engine: Mutex<Option<Engine>>,
    pusher: Pusher,
    events: Receiver<Event>,
    /// The session's audio, exactly as pushed, so the file's time zero is
    /// the transcript's. Closed by `finish`.
    recorder: Mutex<Option<OpusWriter>>,
}

/// Load the recognizers, and the translators for `others` into `native`.
///
/// `streaming` is `language=directory` per pass-1 model, comma
/// separated, the first being the language pass 1 starts in. It gets
/// more than one because it is the one pass that has to be told which
/// language to listen in -- a streaming model is chosen before anything
/// has been said -- so it is given them all and moves between them as
/// pass 2 says which language is being spoken. `others` empty is
/// translation off.
fn load(
    dir: &Path,
    threads: i32,
    streaming: &str,
    native: &str,
    others: &[String],
) -> Result<Models> {
    // Before anything is loaded: the languages are a settings mistake or
    // they are not, and saying which should not cost the seconds the
    // recognizers take. (vox_scribe builds its translation pass first for
    // the same reason.)
    check_languages(dir, native, others)?;
    let mut pc = ParakeetConfig::from_dir(&dir.join(PARAKEET));
    pc.num_threads = threads;
    let offline = Arc::new(Parakeet::open(&pc).context("loading Parakeet")?);
    let mut pass1 = Vec::new();
    for (language, bundle) in parse_streaming(streaming)? {
        let mut zc = ZipformerConfig::from_dir(&dir.join(&bundle));
        zc.num_threads = threads;
        let z = Zipformer::open(&zc).with_context(|| format!("loading {bundle}"))?;
        pass1.push((language, z));
    }
    // Which pass-1 models these are, in logcat: pass 1 moves between
    // them on its own, and "the live text is in the wrong language" is
    // otherwise a thing only the person listening can tell you.
    tracing::info!(pass1 = %streaming, read_in = %native, "models loaded");
    let (first, zipformer_also) = {
        let mut it = pass1.into_iter();
        let first = it.next().expect("parse_streaming returns at least one");
        (first.1, it.collect::<Vec<_>>())
    };
    let zipformer = first;
    let translate = load_translating(dir, native, others)?;
    Ok(Models {
        offline,
        zipformer,
        zipformer_also,
        translate,
    })
}

/// `language=directory` per entry, comma separated; the first is the
/// language pass 1 starts in.
fn parse_streaming(spec: &str) -> Result<Vec<(String, String)>> {
    let list: Vec<(String, String)> = spec
        .split(',')
        .filter_map(|e| e.split_once('='))
        .map(|(l, d)| (l.trim().to_string(), d.trim().to_string()))
        .filter(|(l, d)| !l.is_empty() && !d.is_empty())
        .collect();
    if list.is_empty() {
        anyhow::bail!("no pass-1 model in {spec:?} (want `language=directory`)");
    }
    Ok(list)
}

/// Whether these languages can be told apart at all, which is all of
/// what `load` can answer before it reads the weights.
///
/// Two languages in one alphabet can be, but only with the classifier
/// downloaded: without it the alphabet is the whole of the decision and
/// the pair is refused rather than guessed at -- see
/// `vox_transcribe::language`. So this is asked of the models directory
/// and not of a build flag, because it is a question about what is on
/// the phone right now.
#[cfg(feature = "translate-local")]
fn check_languages(dir: &Path, native: &str, others: &[String]) -> Result<()> {
    if others.is_empty() {
        return Ok(());
    }
    let telling = vox_transcribe::langid::FastText::installed(dir);
    vox_transcribe::language::pair(native, others, telling)
}

#[cfg(not(feature = "translate-local"))]
fn check_languages(_dir: &Path, _native: &str, others: &[String]) -> Result<()> {
    if others.is_empty() {
        return Ok(());
    }
    anyhow::bail!("this build has no translator (the `translate-local` feature)")
}

/// The translators, loaded once for every session to share. The slow
/// part: each direction is about 80 MB of weights.
#[cfg(feature = "translate-local")]
fn load_translating(dir: &Path, native: &str, others: &[String]) -> Result<Option<Translating>> {
    if others.is_empty() {
        return Ok(None);
    }
    let mut langs = vec![native.to_string()];
    langs.extend(others.iter().cloned());
    let by = vox_transcribe::ct2::Ct2Translator::load(dir, &langs)?;
    tracing::info!(directions = %by.directions().join(", "), "translating locally");
    // Loaded if it is there, and not required: a set the alphabet can
    // separate needs no classifier, and `check_languages` has already
    // refused any set that does need one when it is missing. 917 KB, so
    // it is loaded whenever it is present rather than only when some
    // pair is about to want it -- working out that nothing needs it
    // would cost more than the loading does.
    let telling = match vox_transcribe::langid::FastText::load(dir) {
        Ok(c) => {
            tracing::info!(by = %vox_transcribe::langid::Classify::name(&c), "telling languages apart");
            Some(Arc::new(c) as Arc<dyn vox_transcribe::langid::Classify>)
        }
        Err(e) => {
            // Not an error to report upwards: a missing classifier is
            // an ordinary state of the models directory, and a corrupt
            // one must not take the recognizers down with it.
            tracing::info!("no language classifier: {e:#}");
            None
        }
    };
    Ok(Some(Translating {
        native: native.to_string(),
        others: others.to_vec(),
        by: Arc::new(by),
        telling,
    }))
}

#[cfg(not(feature = "translate-local"))]
fn load_translating(_dir: &Path, _native: &str, _others: &[String]) -> Result<Option<Translating>> {
    // `check_languages` has already refused anything but "none".
    Ok(None)
}

/// The translation pass for one session, reading the models [`Models`]
/// holds. One per session: an `Interpreter` remembers what it has
/// already read.
#[cfg(feature = "translate-local")]
fn interpreter(models: &Models) -> Result<Option<vox_transcribe::language::Interpreter>> {
    let Some(t) = &models.translate else {
        return Ok(None);
    };
    Ok(Some(vox_transcribe::language::Interpreter::new(
        &t.native,
        &t.others,
        t.by.clone(),
        t.telling.clone(),
    )?))
}

#[cfg(not(feature = "translate-local"))]
fn interpreter(_models: &Models) -> Result<Option<vox_transcribe::language::Interpreter>> {
    Ok(None)
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
    let file =
        std::fs::File::open(archive).with_context(|| format!("open {}", archive.display()))?;
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
        Change::Translated => "translated",
        Change::Hardened => "hardened",
    }
}

/// Everything queued, waiting up to `timeout` for the first event. Level
/// events are collapsed to the latest one, and so is every update of one
/// paragraph: each event carries the whole paragraph, so only its last
/// state is news to the app. Without that, a backlog the app has not
/// caught up with grows without bound -- a long paragraph is megabytes
/// of JSON, and a labelled recording resends every piece of it on each
/// update -- until one drain is more than a Java string can hold.
fn drain(events: &Receiver<Event>, timeout: Duration) -> Value {
    // Each paragraph keeps the place its first event had, so the order
    // of what is left still holds; `None` is one a removal dropped.
    let mut out: Vec<Option<Value>> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    let mut level = None;
    let first = events.recv_timeout(timeout).ok();
    for ev in first.into_iter().chain(events.try_iter()) {
        match ev {
            Event::Paragraph { paragraph, change } => {
                let i = *at.entry(paragraph.id.clone()).or_insert_with(|| {
                    out.push(None);
                    out.len() - 1
                });
                // The app reads `change` only to tell a partial from a
                // finished paragraph, and a finish collapsed into a
                // later partial would be a save it never makes: the
                // stronger of the two names stays.
                let name = out[i]
                    .as_ref()
                    .and_then(|old| old["change"].as_str())
                    .filter(|was| *was != "partial" && matches!(change, Change::Partial))
                    .map_or_else(|| change_name(change).to_owned(), str::to_owned);
                out[i] = Some(json!({
                    "t": "paragraph",
                    "change": name,
                    "p": paragraph,
                }));
            }
            Event::ParagraphRemoved { id } => {
                // Whatever was still waiting to be sent of it is moot.
                if let Some(i) = at.remove(&id) {
                    out[i] = None;
                }
                out.push(Some(json!({ "t": "removed", "id": id })));
            }
            Event::Level { rms, speaking, .. } => {
                level = Some(json!({ "t": "level", "rms": rms, "speaking": speaking }))
            }
        }
    }
    let mut out: Vec<Value> = out.into_iter().flatten().collect();
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

/// `load(modelsDir, threads, pass1Models, nativeLanguage, otherLanguages)`:
/// a models handle, or throws. `pass1Models` is `language=directory` per
/// pass-1 model, the first being the one pass 1 starts in;
/// `otherLanguages` is comma-separated and may be empty, which is
/// translation off.
#[no_mangle]
pub extern "system" fn Java_com_enigmacurry_rpg_1vox_1scribe_Native_load(
    mut env: JNIEnv,
    _class: JClass,
    dir: JString,
    threads: jint,
    streaming: JString,
    native: JString,
    others: JString,
) -> jlong {
    init_logging();
    let args: Result<(String, String, String, String)> = (|| {
        Ok((
            env.get_string(&dir)?.into(),
            env.get_string(&streaming)?.into(),
            env.get_string(&native)?.into(),
            env.get_string(&others)?.into(),
        ))
    })();
    let (dir, streaming, native, others) = match args {
        Ok(a) => a,
        Err(e) => {
            throw(&mut env, &e.context("load arguments"));
            return 0;
        }
    };
    let others: Vec<String> = others
        .split(',')
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    match load(Path::new(&dir), threads, &streaming, &native, &others) {
        Ok(m) => Box::into_raw(Box::new(m)) as jlong,
        Err(e) => {
            throw(&mut env, &e);
            0
        }
    }
}

/// `writeTokenizer(spm, out)`: the `tokenizer.json` a Marian model is
/// read with, written from the `source.spm` it ships. Returns how many
/// pieces it has; throws on failure.
///
/// The models are downloaded by Kotlin (`ModelStore`), which can do
/// everything but this: see `vox_transcribe::spm` for why there is no
/// `tokenizer.json` to download and no SentencePiece here to make one
/// with.
#[no_mangle]
pub extern "system" fn Java_com_enigmacurry_rpg_1vox_1scribe_Native_writeTokenizer(
    mut env: JNIEnv,
    _class: JClass,
    spm: JString,
    out: JString,
) -> jint {
    init_logging();
    let args: Result<(String, String)> =
        (|| Ok((env.get_string(&spm)?.into(), env.get_string(&out)?.into())))();
    match args.and_then(|(spm, out)| write_tokenizer(Path::new(&spm), Path::new(&out))) {
        Ok(pieces) => pieces as jint,
        Err(e) => {
            throw(&mut env, &e);
            0
        }
    }
}

#[cfg(feature = "translate-local")]
fn write_tokenizer(spm: &Path, out: &Path) -> Result<usize> {
    vox_transcribe::spm::write_tokenizer(spm, out)
}

#[cfg(not(feature = "translate-local"))]
fn write_tokenizer(_spm: &Path, _out: &Path) -> Result<usize> {
    anyhow::bail!("this build has no translator (the `translate-local` feature)")
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

/// `start(models, sampleRate, streaming, opusPath)`: a session handle
/// for one source. Without streaming (files) there are no pass-1
/// partials. With `opusPath` (may be null) the audio is also written
/// there; throws if it can't be created.
#[no_mangle]
pub extern "system" fn Java_com_enigmacurry_rpg_1vox_1scribe_Native_start(
    mut env: JNIEnv,
    _class: JClass,
    models: jlong,
    rate: jint,
    streaming: jboolean,
    opus_path: JString,
) -> jlong {
    let models = unsafe { &*(models as *const Models) };
    let recorder = if opus_path.is_null() {
        None
    } else {
        let created = env
            .get_string(&opus_path)
            .map(String::from)
            .map_err(anyhow::Error::from)
            .and_then(|p| OpusWriter::create(Path::new(&p), rate as u32));
        match created {
            Ok(w) => Some(w),
            Err(e) => {
                throw(&mut env, &e);
                return 0;
            }
        }
    };
    let streaming = (streaming != 0)
        .then(|| Box::new(models.zipformer.session()) as Box<dyn StreamingRecognizer>);
    // The languages were settled when the models loaded, so this can
    // only fail on something that was already reported then.
    let interpreter = match interpreter(models) {
        Ok(i) => i,
        Err(e) => {
            throw(&mut env, &e);
            return 0;
        }
    };
    // Pass 1's other languages, when there is a pass 1 at all: a file
    // has no live text to follow the speaking with.
    let streaming_also = match streaming.is_some() {
        true => models
            .zipformer_also
            .iter()
            .map(|(language, z)| {
                let by = Box::new(z.session()) as Box<dyn StreamingRecognizer>;
                (language.clone(), by)
            })
            .collect(),
        false => Vec::new(),
    };
    let engine = Engine::spawn_passes(
        EngineConfig::new(rate as u32),
        streaming,
        models.offline.clone(),
        vox_transcribe::engine::Passes {
            interpreter,
            streaming_also,
            ..Default::default()
        },
    );
    let events = engine.events().clone();
    let pusher = engine.pusher();
    let session = Session {
        engine: Mutex::new(Some(engine)),
        pusher,
        events,
        recorder: Mutex::new(recorder),
    };
    Box::into_raw(Box::new(session)) as jlong
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
        session.pusher.push(&buf);
        let mut recorder = session.recorder.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(w) = recorder.as_mut() {
            if let Err(e) = w.write(&buf) {
                tracing::warn!("recording stopped: {e:#}");
                *recorder = None;
            }
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_com_enigmacurry_rpg_1vox_1scribe_Native_breakParagraph(
    _env: JNIEnv,
    _class: JClass,
    session: jlong,
) {
    let session = unsafe { &*(session as *const Session) };
    session.pusher.break_paragraph();
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
/// as JSON (`{"paragraphs": [...]}`). Events keep arriving on `poll`
/// meanwhile. The session stays allocated until `free`.
#[no_mangle]
pub extern "system" fn Java_com_enigmacurry_rpg_1vox_1scribe_Native_finish(
    mut env: JNIEnv,
    _class: JClass,
    session: jlong,
) -> jstring {
    let session = unsafe { &*(session as *const Session) };
    let recorder = session
        .recorder
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    if let Some(Err(e)) = recorder.map(OpusWriter::finish) {
        tracing::warn!("closing recording: {e:#}");
    }
    let engine = session
        .engine
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    let transcript = engine.map(Engine::finish).unwrap_or_default();
    let json = serde_json::to_string(&transcript).unwrap_or_else(|_| "{}".into());
    new_string(&mut env, &json)
}

#[no_mangle]
pub extern "system" fn Java_com_enigmacurry_rpg_1vox_1scribe_Native_free(
    _env: JNIEnv,
    _class: JClass,
    session: jlong,
) {
    if session != 0 {
        drop(unsafe { Box::from_raw(session as *mut Session) });
    }
}

/// Sequential reader over a recording, for playback from Kotlin.
struct Player {
    /// Borrows `file`; declared first so it is dropped before it.
    cursor: Option<Cursor<'static>>,
    /// Decoded samples not yet handed out.
    pending: Vec<f32>,
    file: Box<OpusFile>,
}

impl Player {
    fn seek(&mut self, ms: u64) -> Result<()> {
        self.cursor = None;
        self.pending.clear();
        let cursor = self.file.cursor(ms * OPUS_RATE as u64 / 1000)?;
        // SAFETY: the cursor borrows the boxed file, whose address is
        // stable and which outlives it (field order, and `seek` drops
        // the old cursor before making a new one).
        self.cursor = Some(unsafe { std::mem::transmute::<Cursor<'_>, Cursor<'static>>(cursor) });
        Ok(())
    }

    /// The stretch from the first to the last 50 ms window louder than
    /// `threshold_db` (RMS, dBFS), in ms, padded by `pad_ms` each side;
    /// `None` if nothing is that loud. Leaves the player at the start.
    fn span(&mut self, threshold_db: f32, pad_ms: u64) -> Result<Option<(u64, u64)>> {
        let window = (OPUS_RATE / 20) as usize;
        let floor = 10f32.powf(threshold_db / 20.0);
        self.seek(0)?;
        let mut buf = vec![0f32; window];
        let (mut first, mut last) = (None, 0u64);
        let mut at = 0u64;
        loop {
            let n = self.read(&mut buf)?;
            if n == 0 {
                break;
            }
            let rms = (buf[..n].iter().map(|s| s * s).sum::<f32>() / n as f32).sqrt();
            if rms > floor {
                first.get_or_insert(at);
                last = at + n as u64;
            }
            at += n as u64;
        }
        self.seek(0)?;
        let ms = |samples: u64| samples * 1000 / OPUS_RATE as u64;
        let len = ms(at);
        Ok(first.map(|f| (ms(f).saturating_sub(pad_ms), (ms(last) + pad_ms).min(len))))
    }

    /// Fill `out` from the cursor; how many samples were written (fewer
    /// only at the end).
    fn read(&mut self, out: &mut [f32]) -> Result<usize> {
        while self.pending.len() < out.len() {
            let Some(c) = self.cursor.as_mut() else { break };
            if !c.read(&mut self.pending)? {
                self.cursor = None;
            }
        }
        let n = self.pending.len().min(out.len());
        out[..n].copy_from_slice(&self.pending[..n]);
        self.pending.drain(..n);
        Ok(n)
    }
}

/// `playerOpen(path)`: a player handle positioned at the start, or throws.
/// Output is mono float at 48 kHz.
#[no_mangle]
pub extern "system" fn Java_com_enigmacurry_rpg_1vox_1scribe_Native_playerOpen(
    mut env: JNIEnv,
    _class: JClass,
    path: JString,
) -> jlong {
    let opened = env
        .get_string(&path)
        .map(String::from)
        .map_err(anyhow::Error::from)
        .and_then(|p| OpusFile::open(Path::new(&p)))
        .and_then(|f| {
            let mut p = Player {
                cursor: None,
                pending: Vec::new(),
                file: Box::new(f),
            };
            p.seek(0)?;
            Ok(p)
        });
    match opened {
        Ok(p) => Box::into_raw(Box::new(p)) as jlong,
        Err(e) => {
            throw(&mut env, &e);
            0
        }
    }
}

/// `alignWords(wordsJson, text, startMs, endMs)`: timings for the words
/// of `text` (an edit of the text `wordsJson` timed), as the same JSON:
/// words that survive keep their times, new ones are spread between
/// (see [`vox_transcribe::timing::align_words`]).
#[no_mangle]
pub extern "system" fn Java_com_enigmacurry_rpg_1vox_1scribe_Native_alignWords(
    mut env: JNIEnv,
    _class: JClass,
    words: JString,
    text: JString,
    start_ms: jlong,
    end_ms: jlong,
) -> jstring {
    let aligned: Result<String> = (|| {
        let words: String = env.get_string(&words)?.into();
        let text: String = env.get_string(&text)?.into();
        let old: Vec<vox_transcribe::Word> = serde_json::from_str(&words)?;
        let span = (start_ms.max(0) as u64, end_ms.max(0) as u64);
        Ok(serde_json::to_string(
            &vox_transcribe::timing::align_words(&old, &text, span),
        )?)
    })();
    match aligned {
        Ok(json) => new_string(&mut env, &json),
        Err(e) => {
            throw(&mut env, &e);
            std::ptr::null_mut()
        }
    }
}

/// `playerSpan(player, thresholdDb, padMs)`: `[startMs, endMs]` of the
/// sound in the recording (see [`Player::span`]), or null if it's all
/// quieter than that. The player is left at the start.
#[no_mangle]
pub extern "system" fn Java_com_enigmacurry_rpg_1vox_1scribe_Native_playerSpan<'a>(
    mut env: JNIEnv<'a>,
    _class: JClass,
    player: jlong,
    threshold_db: jfloat,
    pad_ms: jlong,
) -> JLongArray<'a> {
    let player = unsafe { &mut *(player as *mut Player) };
    match player.span(threshold_db, pad_ms.max(0) as u64) {
        Ok(Some((a, b))) => {
            let Ok(arr) = env.new_long_array(2) else {
                return JLongArray::default();
            };
            let _ = env.set_long_array_region(&arr, 0, &[a as jlong, b as jlong]);
            arr
        }
        Ok(None) => JLongArray::default(),
        Err(e) => {
            throw(&mut env, &e);
            JLongArray::default()
        }
    }
}

/// Length of the recording in ms.
#[no_mangle]
pub extern "system" fn Java_com_enigmacurry_rpg_1vox_1scribe_Native_playerLength(
    _env: JNIEnv,
    _class: JClass,
    player: jlong,
) -> jlong {
    let player = unsafe { &*(player as *const Player) };
    (player.file.len * 1000 / OPUS_RATE as u64) as jlong
}

#[no_mangle]
pub extern "system" fn Java_com_enigmacurry_rpg_1vox_1scribe_Native_playerSeek(
    mut env: JNIEnv,
    _class: JClass,
    player: jlong,
    ms: jlong,
) {
    let player = unsafe { &mut *(player as *mut Player) };
    if let Err(e) = player.seek(ms.max(0) as u64) {
        throw(&mut env, &e);
    }
}

/// `playerRead(player, out)`: samples written to `out`, 0 at the end.
#[no_mangle]
pub extern "system" fn Java_com_enigmacurry_rpg_1vox_1scribe_Native_playerRead(
    mut env: JNIEnv,
    _class: JClass,
    player: jlong,
    out: JFloatArray,
) -> jint {
    let player = unsafe { &mut *(player as *mut Player) };
    let len = env.get_array_length(&out).unwrap_or(0).max(0) as usize;
    let mut buf = vec![0f32; len];
    match player.read(&mut buf) {
        Ok(n) => {
            if env.set_float_array_region(&out, 0, &buf[..n]).is_err() {
                return 0;
            }
            n as jint
        }
        Err(e) => {
            throw(&mut env, &e);
            0
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_com_enigmacurry_rpg_1vox_1scribe_Native_playerFree(
    _env: JNIEnv,
    _class: JClass,
    player: jlong,
) {
    if player != 0 {
        drop(unsafe { Box::from_raw(player as *mut Player) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_seeks_and_reads_a_recording() {
        let dir = std::env::temp_dir().join(format!("vox_android_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.opus");
        // 2 s at 16 kHz: silence with a click at exactly 1.5 s.
        let mut input = vec![0.0f32; 32_000];
        for s in &mut input[24_000..24_016] {
            *s = 0.9;
        }
        let mut w = OpusWriter::create(&path, 16_000).unwrap();
        for chunk in input.chunks(1600) {
            w.write(chunk).unwrap();
        }
        w.finish().unwrap();

        let mut p = Player {
            cursor: None,
            pending: Vec::new(),
            file: Box::new(OpusFile::open(&path).unwrap()),
        };
        p.seek(1000).unwrap();
        let mut out = Vec::new();
        let mut buf = vec![0f32; 2400];
        loop {
            let n = p.read(&mut buf).unwrap();
            out.extend_from_slice(&buf[..n]);
            if n < buf.len() {
                break;
            }
        }
        // 1 s left, the click 0.5 s in.
        assert!((out.len() as i64 - 48_000).abs() <= 3, "{}", out.len());
        let peak = (0..out.len())
            .max_by(|&a, &b| out[a].abs().total_cmp(&out[b].abs()))
            .unwrap();
        assert!((peak as i64 - 24_000).abs() < 200, "click at {peak}");
        assert_eq!(p.read(&mut buf).unwrap(), 0);
        // The click is the only sound: 1.5 s, padded 100 ms each side, to
        // within a 50 ms window (Opus smears it a little).
        let (a, b) = p.span(-40.0, 100).unwrap().unwrap();
        assert!((1340..=1410).contains(&a), "{a}");
        assert!((1640..=1710).contains(&b), "{b}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// What `Native.load` does, with the real models: both languages
    /// the app can read in, each with the other listened out for, and
    /// then the per-session `Interpreter` the engine is given. The
    /// models are the phone's, so this is the one place the three of
    /// them are asked to load together off a phone.
    ///
    /// Not run by default: it wants every model downloaded.
    ///
    /// ```text
    /// VOX_SCRIBE_MODELS=models cargo test -p vox_android --features translate-local \
    ///   -- --ignored --nocapture languages
    /// ```
    #[test]
    #[ignore = "loads the downloaded recognizers and translation models"]
    #[cfg(feature = "translate-local")]
    fn loads_the_languages_the_app_offers() {
        let dir = std::env::var("VOX_SCRIBE_MODELS").expect("VOX_SCRIBE_MODELS");
        let dir = Path::new(&dir);
        for (streaming, native, other) in [
            (
                "English=streaming-zipformer,Russian=streaming-zipformer-ru",
                "English",
                "Russian",
            ),
            (
                "Russian=streaming-zipformer-ru,English=streaming-zipformer",
                "Russian",
                "English",
            ),
        ] {
            let others = vec![other.to_string()];
            let models = load(dir, 2, streaming, native, &others)
                .unwrap_or_else(|e| panic!("load {native} with {other}: {e:#}"));
            assert!(models.translate.is_some(), "{native}: no translator");
            assert_eq!(
                models.zipformer_also.len(),
                1,
                "{native}: pass 1 should have the other language to move to"
            );
            let i = interpreter(&models)
                .unwrap_or_else(|e| panic!("interpreter for {native}: {e:#}"))
                .unwrap_or_else(|| panic!("{native}: no interpreter"));
            assert_eq!(i.into_language(), native);
            println!("{native} reading {other}: {i:?}");
        }
    }

    /// A pair the alphabet cannot decide between is refused by `load`,
    /// which is why the settings page only offers one language per
    /// alphabet. Needs no models: the check comes first. Without the
    /// feature there is no translator to refuse for, and `load` says
    /// that instead -- which the build the app ships never does.
    #[test]
    #[cfg(feature = "translate-local")]
    fn refuses_two_languages_in_one_alphabet() {
        let refused = load(
            Path::new("/nonexistent"),
            1,
            "English=streaming-zipformer",
            "English",
            &["German".to_string()],
        );
        let Err(e) = refused else {
            panic!("German against English is Latin against Latin");
        };
        let said = format!("{e:#}");
        assert!(
            said.contains("alphabet") || said.contains("Latin"),
            "it should say why, not just fail: {said}"
        );
    }
}
