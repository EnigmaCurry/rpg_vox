//! C ABI for the iOS app (`ios/`), the counterpart of crates/vox_android:
//! loads Parakeet (passes 2 and 3) and Zipformer (pass 1) from a models
//! directory, runs a [`vox_transcribe::Engine`] fed with samples from
//! Swift, and hands its events back as JSON. A session can also keep its
//! audio as Ogg Opus, and [`Player`] reads such a file back for playback.
//! The declarations Swift sees are in `ios/Scribe/vox_ios.h`.
//!
//! Every handle is a `Box` pointer. The Swift side owns the order of
//! calls: nothing on a session after `vox_free`, no `vox_start` after
//! `vox_free_models`. `vox_push` and `vox_poll` may run alongside
//! `vox_finish`. Strings returned to Swift are freed with
//! `vox_string_free`; a function that can fail returns null/false and,
//! when `err` isn't null, sets `*err` to the reason.

use std::collections::HashMap;
use std::ffi::{c_char, CStr, CString};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use anyhow::{Context as _, Result};
use crossbeam_channel::Receiver;
use serde_json::{json, Value};
use vox_audio::opus_file::{Cursor, OpusFile, OpusWriter, RATE as OPUS_RATE};
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
    /// The session's audio, exactly as pushed, so the file's time zero is
    /// the transcript's. Closed by `vox_finish`.
    recorder: Mutex<Option<OpusWriter>>,
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

unsafe fn path(s: *const c_char) -> PathBuf {
    PathBuf::from(CStr::from_ptr(s).to_string_lossy().into_owned())
}

unsafe fn str<'a>(s: *const c_char) -> std::borrow::Cow<'a, str> {
    CStr::from_ptr(s).to_string_lossy()
}

/// Logs `e` and hands it to `err`, if wanted.
unsafe fn fail(err: *mut *mut c_char, e: &anyhow::Error) {
    tracing::error!("{e:#}");
    if !err.is_null() {
        *err = c_string(&format!("{e:#}"));
    }
}

/// Extracts entries from a `.tar.bz2` into `dest`; `spec` has one
/// `<path below the top folder>\t<installed name>` per line. Blocks.
///
/// # Safety
/// Strings are NUL-terminated; `err` is null or writable.
#[no_mangle]
pub unsafe extern "C" fn vox_unpack(
    archive: *const c_char,
    dest: *const c_char,
    spec: *const c_char,
    err: *mut *mut c_char,
) -> bool {
    init_logging();
    match unpack(&path(archive), &path(dest), &str(spec)) {
        Ok(()) => true,
        Err(e) => {
            fail(err, &e);
            false
        }
    }
}

/// Compressed bytes the running `vox_unpack` has read so far.
#[no_mangle]
pub extern "C" fn vox_unpack_progress() -> u64 {
    UNPACKED.load(Ordering::Relaxed)
}

/// Loads the models from `dir`: a models handle, or null.
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
    match load(&path(dir), threads) {
        Ok(m) => Box::into_raw(Box::new(m)),
        Err(e) => {
            fail(err, &e);
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
/// `streaming` (files) there are no pass-1 partials. With `opus_path`
/// (may be null) the audio is also written there; null if it can't be
/// created.
///
/// # Safety
/// `models` is from `vox_load` and outlives the session; `opus_path` is
/// null or NUL-terminated; `err` is null or writable.
#[no_mangle]
pub unsafe extern "C" fn vox_start(
    models: *const Models,
    rate: u32,
    streaming: bool,
    opus_path: *const c_char,
    err: *mut *mut c_char,
) -> *mut Session {
    let models = &*models;
    let recorder = if opus_path.is_null() {
        None
    } else {
        match OpusWriter::create(&path(opus_path), rate) {
            Ok(w) => Some(w),
            Err(e) => {
                fail(err, &e);
                return std::ptr::null_mut();
            }
        }
    };
    let streaming =
        streaming.then(|| Box::new(models.zipformer.session()) as Box<dyn StreamingRecognizer>);
    let engine = Engine::spawn(EngineConfig::new(rate), streaming, models.offline.clone());
    let events = engine.events().clone();
    let pusher = engine.pusher();
    Box::into_raw(Box::new(Session {
        engine: Mutex::new(Some(engine)),
        pusher,
        events,
        recorder: Mutex::new(recorder),
    }))
}

/// # Safety
/// `session` is from `vox_start`; `samples` points at `len` floats.
#[no_mangle]
pub unsafe extern "C" fn vox_push(session: *const Session, samples: *const f32, len: usize) {
    if samples.is_null() || len == 0 {
        return;
    }
    let session = &*session;
    let buf = std::slice::from_raw_parts(samples, len);
    session.pusher.push(buf);
    let mut recorder = session.recorder.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(w) = recorder.as_mut() {
        if let Err(e) = w.write(buf) {
            tracing::warn!("recording stopped: {e:#}");
            *recorder = None;
        }
    }
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

/// Closes the recording, drains every pass and returns the final
/// transcript as JSON (`{"paragraphs": [...]}`). Events keep arriving on
/// `vox_poll` meanwhile. The session stays allocated until `vox_free`.
///
/// # Safety
/// `session` is from `vox_start`.
#[no_mangle]
pub unsafe extern "C" fn vox_finish(session: *const Session) -> *mut c_char {
    let session = &*session;
    let recorder = session.recorder.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(Err(e)) = recorder.map(OpusWriter::finish) {
        tracing::warn!("closing recording: {e:#}");
    }
    let engine = session.engine.lock().unwrap_or_else(|e| e.into_inner()).take();
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

/// Timings for the words of `text` (an edit of the text `words_json`
/// timed), as the same JSON: words that survive keep their times, new
/// ones are spread between (see [`vox_transcribe::timing::align_words`]).
/// Null on bad JSON.
///
/// # Safety
/// Strings are NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn vox_align_words(
    words_json: *const c_char,
    text: *const c_char,
    start_ms: u64,
    end_ms: u64,
) -> *mut c_char {
    let aligned: Result<String> = (|| {
        let old: Vec<vox_transcribe::Word> = serde_json::from_str(&str(words_json))?;
        let words = vox_transcribe::timing::align_words(&old, &str(text), (start_ms, end_ms));
        Ok(serde_json::to_string(&words)?)
    })();
    match aligned {
        Ok(json) => c_string(&json),
        Err(e) => {
            tracing::warn!("align words: {e:#}");
            std::ptr::null_mut()
        }
    }
}

/// Sequential reader over a recording, for playback from Swift.
pub struct Player {
    /// Borrows `file`; declared first so it is dropped before it.
    cursor: Option<Cursor<'static>>,
    /// Decoded samples not yet handed out.
    pending: Vec<f32>,
    file: Box<OpusFile>,
}

impl Player {
    fn open(path: &Path) -> Result<Self> {
        let mut p = Player {
            cursor: None,
            pending: Vec::new(),
            file: Box::new(OpusFile::open(path)?),
        };
        p.seek(0)?;
        Ok(p)
    }

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

/// A player positioned at the start of the recording at `path`, or null.
/// Output is mono float at 48 kHz.
///
/// # Safety
/// `path` is NUL-terminated; `err` is null or writable.
#[no_mangle]
pub unsafe extern "C" fn vox_player_open(path_: *const c_char, err: *mut *mut c_char) -> *mut Player {
    match Player::open(&path(path_)) {
        Ok(p) => Box::into_raw(Box::new(p)),
        Err(e) => {
            fail(err, &e);
            std::ptr::null_mut()
        }
    }
}

/// `[*start_ms, *end_ms]` of the sound in the recording (see
/// [`Player::span`]); false if it's all quieter than that or unreadable.
/// The player is left at the start.
///
/// # Safety
/// `player` is from `vox_player_open`; the outputs are writable.
#[no_mangle]
pub unsafe extern "C" fn vox_player_span(
    player: *mut Player,
    threshold_db: f32,
    pad_ms: u64,
    start_ms: *mut u64,
    end_ms: *mut u64,
) -> bool {
    match (*player).span(threshold_db, pad_ms) {
        Ok(Some((a, b))) => {
            *start_ms = a;
            *end_ms = b;
            true
        }
        Ok(None) => false,
        Err(e) => {
            tracing::warn!("player span: {e:#}");
            false
        }
    }
}

/// Length of the recording in ms.
///
/// # Safety
/// `player` is from `vox_player_open`.
#[no_mangle]
pub unsafe extern "C" fn vox_player_length(player: *const Player) -> u64 {
    (*player).file.len * 1000 / OPUS_RATE as u64
}

/// # Safety
/// `player` is from `vox_player_open`.
#[no_mangle]
pub unsafe extern "C" fn vox_player_seek(player: *mut Player, ms: u64) -> bool {
    match (*player).seek(ms) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!("player seek: {e:#}");
            false
        }
    }
}

/// Samples written to `out` (at most `len`), 0 at the end or on error.
///
/// # Safety
/// `player` is from `vox_player_open`; `out` has room for `len` floats.
#[no_mangle]
pub unsafe extern "C" fn vox_player_read(player: *mut Player, out: *mut f32, len: usize) -> usize {
    let out = std::slice::from_raw_parts_mut(out, len);
    match (*player).read(out) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!("player read: {e:#}");
            0
        }
    }
}

/// # Safety
/// `player` is null or from `vox_player_open`, and not used afterwards.
#[no_mangle]
pub unsafe extern "C" fn vox_player_free(player: *mut Player) {
    if !player.is_null() {
        drop(Box::from_raw(player));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_plays_back_through_the_c_abi() {
        let dir = std::env::temp_dir().join(format!("vox_ios_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("t.opus");
        // 2 s at 16 kHz: silence with a click at exactly 1.5 s.
        let mut input = vec![0.0f32; 32_000];
        for s in &mut input[24_000..24_016] {
            *s = 0.9;
        }
        let mut w = OpusWriter::create(&file, 16_000).unwrap();
        for chunk in input.chunks(1600) {
            w.write(chunk).unwrap();
        }
        w.finish().unwrap();

        let c = CString::new(file.to_str().unwrap()).unwrap();
        unsafe {
            let p = vox_player_open(c.as_ptr(), std::ptr::null_mut());
            assert!(!p.is_null());
            assert!((vox_player_length(p) as i64 - 2000).abs() <= 1);
            let (mut a, mut b) = (0, 0);
            assert!(vox_player_span(p, -40.0, 100, &mut a, &mut b));
            assert!((1340..=1410).contains(&a), "{a}");
            assert!((1640..=1710).contains(&b), "{b}");
            assert!(vox_player_seek(p, 1000));
            let mut out = vec![0f32; 4800];
            let mut total = 0;
            loop {
                let n = vox_player_read(p, out.as_mut_ptr(), out.len());
                total += n;
                if n == 0 {
                    break;
                }
            }
            assert!((total as i64 - 48_000).abs() <= 3, "{total}");
            vox_player_free(p);
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
