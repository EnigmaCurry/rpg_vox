//! `--jsonl`: live dictation for another program, which starts scribe,
//! reads transcript events on stdout and sends commands on stdin, one
//! JSON object per line each way (the protocol is in SCRIBE_REFERENCE.md).
//!
//! Each paragraph is one segment. While any pass may still change it,
//! its text is part of the `partial`; once it hardens it is sent as a
//! `final` and never touched again. Paragraphs are committed in order,
//! so a later one that hardens first waits in the partial for the one
//! before it.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead as _, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::Result;
use crossbeam_channel::{unbounded, Receiver};
use serde_json::{json, Value};
use tracing::{info, warn};
use vox_transcribe::{Event, Paragraph};

use crate::{Outcome, Pass1, Session};

/// Where events go: the real stdout, taken over by [`claim_stdout`] so
/// nothing else can write to it.
static OUT: OnceLock<Mutex<Box<dyn std::io::Write + Send>>> = OnceLock::new();

/// Take stdout for the protocol; anything else printed goes to stderr.
pub fn claim_stdout() {
    #[cfg(any(unix, windows))]
    let out: Box<dyn std::io::Write + Send> = match vox_transcribe::stderr::take_stdout() {
        Ok(f) => Box::new(f),
        Err(e) => {
            eprintln!("--jsonl: couldn't set stdout aside ({e}); using it as is");
            Box::new(std::io::stdout())
        }
    };
    #[cfg(not(any(unix, windows)))]
    let out: Box<dyn std::io::Write + Send> = Box::new(std::io::stdout());
    let _ = OUT.set(Mutex::new(out));
}

/// Write one event line. The reader going away ends scribe: nobody is
/// left to hear the rest.
pub fn emit(mut event: Value) {
    if let Some(obj) = event.as_object_mut() {
        obj.insert("timestamp".into(), json!(chrono::Utc::now().to_rfc3339()));
    }
    let Some(out) = OUT.get() else {
        return;
    };
    let mut out = out.lock().unwrap_or_else(|e| e.into_inner());
    let line = event.to_string();
    if writeln!(out, "{line}").and_then(|_| out.flush()).is_err() {
        std::process::exit(1);
    }
}

/// Stdout is the protocol's: scribe was started with --jsonl.
pub fn claimed() -> bool {
    OUT.get().is_some()
}

/// A fatal error before or during the session: say so, then end it.
pub fn fail(e: &anyhow::Error) {
    emit(json!({"type": "error", "code": "fatal", "message": format!("{e:#}"), "fatal": true}));
    emit(json!({"type": "session_end", "reason": "error"}));
}

/// What `ready` reports, and how the session runs.
pub struct Ready {
    pub model: String,
    pub sample_rate_hz: u32,
    /// --standby: wait for `start`, and keep running between turns.
    pub standby: bool,
    /// --auto-finish: quiet that finishes a turn by itself.
    pub auto_finish_ms: Option<u64>,
    /// The paragraph gap: quiet before the last words settle.
    pub gap_ms: u64,
}

/// A line read from stdin.
enum Command {
    /// --standby: listen, finishing by itself after this much quiet;
    /// or start paused, with the mic released until `resume`.
    Start(Option<u64>, bool, Option<Value>),
    Finish(Option<Value>),
    Cancel(Option<Value>),
    Stop(Option<Value>),
    Ping(Option<Value>),
    DeleteWord(Option<Value>),
    SetText(String, Option<Value>),
    Reset(Option<Value>),
    Pause(Option<Value>),
    Resume(Option<Value>),
    /// stdin closed.
    Eof,
}

/// Read commands off stdin; a line that isn't one gets an `error` back
/// right away.
fn read_commands() -> Receiver<Command> {
    let (tx, rx) = unbounded();
    std::thread::Builder::new()
        .name("jsonl-stdin".into())
        .spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                let cmd: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(e) => {
                        emit(json!({"type": "error", "code": "bad_command", "message": format!("not JSON: {e}"), "fatal": false}));
                        continue;
                    }
                };
                let id = cmd.get("request_id").cloned();
                let c = match cmd.get("type").and_then(Value::as_str) {
                    Some("start") => Command::Start(
                        cmd.get("auto_finish_ms").and_then(Value::as_u64),
                        cmd.get("paused").and_then(Value::as_bool).unwrap_or(false),
                        id,
                    ),
                    Some("finish") => Command::Finish(id),
                    Some("cancel") => Command::Cancel(id),
                    Some("stop") => Command::Stop(id),
                    Some("ping") => Command::Ping(id),
                    Some("delete_word") => Command::DeleteWord(id),
                    Some("reset") => Command::Reset(id),
                    Some("pause") => Command::Pause(id),
                    Some("resume") => Command::Resume(id),
                    Some("set_text") => match cmd.get("text").and_then(Value::as_str) {
                        Some(text) => Command::SetText(text.to_string(), id),
                        None => {
                            emit(json!({"type": "error", "code": "bad_command", "message": "set_text needs a text string", "fatal": false, "request_id": id}));
                            continue;
                        }
                    },
                    other => {
                        let mut e = json!({"type": "error", "code": "unknown_command", "message": format!("unknown command type {other:?}"), "fatal": false});
                        if let Some(id) = id {
                            e["request_id"] = id;
                        }
                        emit(e);
                        continue;
                    }
                };
                if tx.send(c).is_err() {
                    return;
                }
            }
            let _ = tx.send(Command::Eof);
        })
        .expect("spawn stdin reader");
    rx
}

/// `event` with the command's `request_id`, if it had one.
fn answer(mut event: Value, id: Option<Value>) -> Value {
    if let Some(id) = id {
        event["request_id"] = id;
    }
    event
}

/// `session_end`, with the command's `request_id` if it had one.
fn session_end(reason: &str, id: Option<Value>) {
    emit(answer(json!({"type": "session_end", "reason": reason}), id));
}

/// A command that doesn't fit the moment.
fn refuse(message: &str, id: Option<Value>) {
    emit(answer(
        json!({"type": "error", "code": "bad_state", "message": message, "fatal": false}),
        id,
    ));
}

/// End at once without draining the passes: a cancel, a stop, a signal.
/// What was already committed stays committed; the rest is dropped.
fn end_now(session: &Session, reason: &str, id: Option<Value>) -> ! {
    session.stop.store(true, Ordering::SeqCst);
    session_end(reason, id);
    info!(reason, "jsonl session ended");
    std::process::exit(0);
}

/// Paragraphs any pass may still change, oldest first, and what has
/// been committed.
///
/// Deleted words (`delete_word`) are kept the way `--dictate` keeps
/// them: by their span of audio, so a later pass that re-decodes the
/// sentence leaves them out too. A pass-1 word has no span yet, so it is
/// hidden by its place in its clip until pass 2 times the clip, and then
/// cut by time like any other.
struct Segments {
    pass1: Pass1,
    open: Vec<Paragraph>,
    committed: HashSet<String>,
    /// Everything sent as `final`, for deleting back into it.
    committed_text: String,
    /// The partial last sent, so an unchanged one isn't repeated.
    partial: String,
    /// Spans of audio (inclusive) whose words were deleted.
    cuts: Vec<(u64, u64)>,
    /// Deleted pass-1 words, by clip: each one's place in the clip and
    /// when it was first heard.
    hidden: HashMap<String, Vec<(usize, u64)>>,
    /// When each word of a pass-1 clip was first heard.
    heard: HashMap<String, Vec<u64>>,
    /// The audio clock, from the engine's level events.
    now_ms: u64,
    /// Speech before this (on the audio clock) is in the text the other
    /// side set with `set_text`, as it was edited there.
    floor_ms: u64,
}

/// A word as shown, and where it came from.
struct Shown {
    text: String,
    from: From,
}

enum From {
    /// A finished word's span of audio.
    Time(u64, u64),
    /// Word `n` of pass-1 clip `id`.
    Pass1(String, usize),
    /// A finished word with no time, which can't be deleted.
    Untimed,
}

/// Whether a word starting at `start` was deleted.
fn is_cut(cuts: &[(u64, u64)], start: u64) -> bool {
    cuts.iter().any(|&(a, b)| (a..=b).contains(&start))
}

/// `text`'s words, with `times` when there is one per word.
fn timed(text: &str, times: &[vox_transcribe::Word]) -> Vec<Shown> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let fits = times.len() == words.len();
    words
        .into_iter()
        .enumerate()
        .map(|(i, w)| Shown {
            text: w.to_string(),
            from: if fits {
                From::Time(times[i].start_ms, times[i].end_ms)
            } else {
                From::Untimed
            },
        })
        .collect()
}

impl Segments {
    fn new(pass1: Pass1) -> Self {
        Self {
            pass1,
            open: Vec::new(),
            committed: HashSet::new(),
            committed_text: String::new(),
            partial: String::new(),
            cuts: Vec::new(),
            hidden: HashMap::new(),
            heard: HashMap::new(),
            now_ms: 0,
            floor_ms: 0,
        }
    }

    fn update(&mut self, p: Paragraph) {
        if self.committed.contains(&p.id) {
            return;
        }
        self.track(&p);
        match self.open.iter_mut().find(|o| o.id == p.id) {
            Some(o) => *o = p,
            None => {
                let at = self.open.partition_point(|o| o.start_ms <= p.start_ms);
                self.open.insert(at, p);
            }
        }
    }

    /// Note when new pass-1 words are heard, and turn the hidden ones of
    /// a clip pass 2 has finished into cuts by time.
    fn track(&mut self, p: &Paragraph) {
        for c in &p.clips {
            if c.is_partial() {
                let n = c.text.split_whitespace().count();
                let heard = self.heard.entry(c.id.clone()).or_default();
                while heard.len() < n {
                    heard.push(self.now_ms);
                }
                continue;
            }
            self.heard.remove(&c.id);
            let Some(hidden) = self.hidden.remove(&c.id) else {
                continue;
            };
            let words = c.timed_words();
            for (_, at_ms) in hidden {
                // Pass 1 shows a word about when it ends, a little before
                // or after; the word pass 2 has ending nearest then.
                let pick = words
                    .iter()
                    .filter(|w| !is_cut(&self.cuts, w.start_ms))
                    .min_by_key(|w| w.end_ms.abs_diff(at_ms));
                if let Some(w) = pick {
                    self.cuts
                        .push((w.start_ms, w.start_ms.max(w.end_ms.saturating_sub(1))));
                }
            }
        }
    }

    fn remove(&mut self, id: &str) {
        self.open.retain(|o| o.id != id);
    }

    /// A paragraph's words as shown, deleted ones left out: finished
    /// clips as they are (or as pass 4 corrected them), pass 1's ALL
    /// CAPS guesses in lowercase (or as --pass1 says).
    fn words(&self, p: &Paragraph) -> Vec<Shown> {
        let all = if !p.clips.iter().any(|c| c.is_partial())
            && p.words.len() == p.text.split_whitespace().count()
        {
            timed(&p.text, &p.words)
        } else {
            let mut all = Vec::new();
            for c in &p.clips {
                if c.is_partial() {
                    let hidden = self.hidden.get(&c.id);
                    let kept: Vec<String> = c
                        .text
                        .split_whitespace()
                        .enumerate()
                        .filter(|(i, _)| !hidden.is_some_and(|h| h.iter().any(|(n, _)| n == i)))
                        .map(|(_, w)| w.to_lowercase())
                        .collect();
                    // Shown words map back to their place in the clip only
                    // as plain text; ellipsis and hidden show no words.
                    let places: Vec<usize> = (0..c.text.split_whitespace().count())
                        .filter(|i| !hidden.is_some_and(|h| h.iter().any(|(n, _)| n == i)))
                        .collect();
                    let shown = self.pass1.words(&kept.join(" "));
                    let plain = self.pass1 == Pass1::Text;
                    for (j, w) in shown.into_iter().enumerate() {
                        let from = match places.get(j) {
                            Some(&n) if plain => From::Pass1(c.id.clone(), n),
                            _ => From::Untimed,
                        };
                        all.push(Shown { text: w, from });
                    }
                } else {
                    all.extend(timed(&c.text, &c.timed_words()));
                }
            }
            all
        };
        all.into_iter()
            .filter(|w| match &w.from {
                From::Time(start, end) => {
                    !is_cut(&self.cuts, *start) && (start + end) / 2 >= self.floor_ms
                }
                // Shown before the text was set: it is in that text.
                From::Pass1(clip, n) => self
                    .heard
                    .get(clip)
                    .and_then(|h| h.get(*n))
                    .is_none_or(|&at| at > self.floor_ms || self.floor_ms == 0),
                From::Untimed => true,
            })
            .collect()
    }

    fn text(&self, p: &Paragraph) -> String {
        self.words(p)
            .into_iter()
            .map(|w| w.text)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Commit the hardened paragraphs at the front, then send what is
    /// left as the partial if it changed.
    fn flush(&mut self) {
        while self.open.first().is_some_and(|p| p.hardened) {
            let p = self.open.remove(0);
            self.commit(&p);
        }
        let partial = self
            .open
            .iter()
            .map(|p| self.text(p))
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let partial = if partial.is_empty() {
            partial
        } else {
            format!("{}{partial}", self.gap())
        };
        if partial != self.partial {
            emit(
                json!({"type": "partial", "text": partial, "segment_id": self.open.first().map(|p| p.id.clone())}),
            );
            self.partial = partial;
        }
    }

    fn commit(&mut self, p: &Paragraph) {
        self.track(p);
        if !self.committed.insert(p.id.clone()) {
            return;
        }
        let text = self.text(p);
        if text.is_empty() {
            return;
        }
        // The trailing space keeps it apart from the next segment.
        let text = format!("{}{text} ", self.gap());
        emit(json!({"type": "final", "text": text, "segment_id": p.id}));
        self.committed_text.push_str(&text);
        // A final clears the partial on the other side.
        self.partial.clear();
    }

    /// A space before new words when the text so far (as `set_text`
    /// left it) doesn't end in one.
    fn gap(&self) -> &'static str {
        match self.committed_text.chars().last() {
            Some(c) if !c.is_whitespace() => " ",
            _ => "",
        }
    }

    /// The other side edited the text: `text` is all of it now, and
    /// speech heard so far is in it as edited. New speech goes after it.
    fn set_text(&mut self, text: String) {
        self.committed_text = text;
        self.floor_ms = self.now_ms;
        // The other side's partial is gone with the edit.
        self.partial.clear();
        self.flush();
    }

    /// Delete the last word: from the partial while there is one, else
    /// from the committed text, which is taken back with `retract`.
    /// False when there is nothing to delete.
    fn delete_word(&mut self) -> bool {
        let last = self.open.iter().rev().find_map(|p| {
            self.words(p)
                .into_iter()
                .rev()
                .find(|w| !matches!(w.from, From::Untimed))
        });
        match last.map(|w| w.from) {
            Some(From::Time(start, end)) => {
                self.cuts.push((start, start.max(end.saturating_sub(1))));
            }
            Some(From::Pass1(clip, n)) => {
                let at = self
                    .heard
                    .get(&clip)
                    .and_then(|h| h.get(n).copied())
                    .unwrap_or(self.now_ms);
                self.hidden.entry(clip).or_default().push((n, at));
            }
            _ if !self.partial.is_empty() => {
                // Words with no time to hold a cut by: nothing to do.
                return false;
            }
            _ => {
                let kept = self.committed_text.trim_end();
                let Some(cut) = kept
                    .rfind(char::is_whitespace)
                    .map(|i| i + 1)
                    .or((!kept.is_empty()).then_some(0))
                else {
                    return false;
                };
                let gone = self.committed_text.split_off(cut);
                emit(json!({"type": "retract", "text": gone}));
                return true;
            }
        }
        self.flush();
        true
    }
}

/// After `resume`, how long before quiet starts to count toward
/// auto-finish.
const RESUME_GRACE_MS: u64 = 1500;

/// One stretch of listening: from `start` (or launch, without
/// --standby) to the end of the dictation.
struct Turn {
    /// Finish by itself after this much quiet, once words were heard.
    auto_finish_ms: Option<u64>,
    /// Some speech has come out as words.
    words: bool,
    /// When speech was last heard, on the audio clock.
    voice_ms: u64,
    /// `pause`: the mic is released until `resume`.
    paused: bool,
}

/// Open the mic and start a turn on a clean slate.
fn begin(
    session: &Session,
    segs: &mut Segments,
    auto_finish_ms: Option<u64>,
    paused: bool,
    standby: bool,
    id: Option<Value>,
) -> Turn {
    segs.set_text(String::new());
    // With --standby, audio from before this turn (heard while the models
    // loaded, or still queued from the last turn) isn't part of it,
    // however far the engine has got with it. Without, speech while the
    // models loaded is the start of the dictation.
    let fed = session
        .slots
        .lock()
        .expect("slots lock")
        .iter()
        .map(|s| s.fed_ms())
        .max()
        .unwrap_or(0);
    if standby {
        segs.floor_ms = segs.floor_ms.max(fed);
    }
    session.paused.store(paused, Ordering::SeqCst);
    let session_id = format!("voice-{}", chrono::Utc::now().to_rfc3339());
    emit(answer(
        json!({"type": "session_start", "session_id": session_id}),
        id,
    ));
    let file = session.input_ms.is_some();
    let message = match (file, paused) {
        (true, _) => "Transcribing",
        (_, true) => "Paused",
        _ => "Listening",
    };
    emit(json!({"type": "status", "level": "info", "message": message}));
    Turn {
        auto_finish_ms,
        words: false,
        voice_ms: segs.now_ms,
        paused,
    }
}

/// Take one engine event into the segments, and the turn if listening.
fn handle(
    ev: Event,
    session: &Session,
    segs: &mut Segments,
    speaking: &mut bool,
    turn: Option<&mut Turn>,
) -> Result<()> {
    match ev {
        Event::Paragraph { paragraph, .. } => {
            let live = paragraph.clips.iter().any(|c| c.is_partial());
            if paragraph.hardened {
                session.write(&paragraph)?;
            }
            segs.update(paragraph);
            segs.flush();
            if let Some(t) = turn {
                t.words |= !segs.partial.is_empty() || !segs.committed_text.trim().is_empty();
                // Pass 1 at work is speech too, and can come before the
                // level event that says so.
                if live {
                    t.voice_ms = t.voice_ms.max(segs.now_ms);
                }
            }
        }
        Event::ParagraphRemoved { id } => {
            segs.remove(&id);
            segs.flush();
        }
        Event::Level {
            speaking: now,
            position_ms,
            ..
        } => {
            segs.now_ms = position_ms;
            if let Some(t) = turn {
                if now {
                    t.voice_ms = position_ms;
                }
            }
            if now != *speaking {
                *speaking = now;
                emit(json!({"type": if now { "vad_start" } else { "vad_end" }}));
            }
        }
    }
    Ok(())
}

/// End a --standby turn and keep running: release the mic, let every
/// pass finish on what was heard, commit it, `session_end`.
#[allow(clippy::too_many_arguments)]
fn finish_turn(
    session: &Session,
    segs: &mut Segments,
    events: &Receiver<Event>,
    speaking: &mut bool,
    gap_ms: u64,
    signalled: &AtomicBool,
    reason: &str,
    id: Option<Value>,
) -> Result<()> {
    session.paused.store(true, Ordering::SeqCst);
    // The last words settle after a paragraph gap of quiet; push that
    // quiet now rather than wait it out on the clock.
    for (i, slot) in session.slots.lock().expect("slots lock").iter().enumerate() {
        let n = (gap_ms + 1000) * slot.rate as u64 / 1000;
        session.engine.pusher(i).push(&vec![0.0; n as usize]);
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !segs.open.is_empty() {
        if signalled.load(Ordering::SeqCst) {
            end_now(session, "sigterm", None);
        }
        if std::time::Instant::now() > deadline {
            warn!("a turn's last passes didn't finish; committing them as they are");
            for p in std::mem::take(&mut segs.open) {
                segs.commit(&p);
            }
            break;
        }
        if let Ok(ev) = events.recv_timeout(Duration::from_millis(50)) {
            handle(ev, session, segs, speaking, None)?;
        }
    }
    if *speaking {
        *speaking = false;
        emit(json!({"type": "vad_end"}));
    }
    session_end(reason, id);
    Ok(())
}

pub fn run(session: Session, ready: Ready) -> Result<Outcome> {
    // SIGTERM (and Ctrl-C) end the session without draining.
    let signalled = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    signal_hook::flag::register(signal_hook::consts::SIGTERM, signalled.clone())?;
    {
        let s = signalled.clone();
        ctrlc::set_handler(move || s.store(true, Ordering::SeqCst))?;
    }
    let commands = read_commands();
    emit(json!({
        "type": "ready",
        "version": 1,
        "engine": "scribe",
        "model": ready.model,
        "sample_rate_hz": ready.sample_rate_hz,
        "standby": ready.standby,
    }));
    let file = session.input_ms.is_some();
    let events = session.engine.events().clone();
    let mut segs = Segments::new(session.pass1);
    let mut speaking = false;
    let mut turn = if ready.standby {
        // The mic is released until `start`.
        session.paused.store(true, Ordering::SeqCst);
        emit(json!({"type": "status", "level": "info", "message": "Standing by"}));
        None
    } else {
        Some(begin(
            &session,
            &mut segs,
            ready.auto_finish_ms,
            false,
            false,
            None,
        ))
    };
    // Why the session is being finished, and the request that asked.
    let (reason, id) = loop {
        if signalled.load(Ordering::SeqCst) {
            end_now(&session, "sigterm", None);
        }
        if session.source_done.load(Ordering::SeqCst) {
            break ("end_of_input", None);
        }
        // The mic couldn't be opened for a turn (the pump gave up).
        if turn.as_ref().is_some_and(|t| !t.paused) && session.paused.load(Ordering::SeqCst) {
            turn = None;
            emit(
                json!({"type": "error", "code": "mic_unavailable", "message": "couldn't open the microphone", "fatal": false}),
            );
            session_end("error", None);
        }
        match commands.try_recv() {
            Ok(Command::Start(auto, paused, id)) => {
                if !ready.standby {
                    refuse(
                        "start needs --standby; this session started listening at launch",
                        id,
                    );
                } else if turn.is_some() {
                    refuse("already listening", id);
                } else {
                    turn = Some(begin(
                        &session,
                        &mut segs,
                        auto.or(ready.auto_finish_ms),
                        paused,
                        true,
                        id,
                    ));
                }
            }
            Ok(Command::Finish(id)) if ready.standby => match turn.take() {
                Some(_) => finish_turn(
                    &session,
                    &mut segs,
                    &events,
                    &mut speaking,
                    ready.gap_ms,
                    &signalled,
                    "finished",
                    id,
                )?,
                None => refuse("not listening", id),
            },
            Ok(Command::Finish(id)) => break ("finished", id),
            Ok(Command::Cancel(id)) if ready.standby => match turn.take() {
                Some(_) => {
                    // Release the mic; what is still open is dropped.
                    session.paused.store(true, Ordering::SeqCst);
                    segs.set_text(String::new());
                    session_end("cancelled", id);
                }
                None => refuse("not listening", id),
            },
            Ok(Command::Cancel(id)) => end_now(&session, "cancelled", id),
            Ok(Command::Stop(id)) => end_now(&session, "stop_requested", id),
            Ok(Command::Pause(id)) => match turn.as_mut() {
                Some(t) => {
                    t.paused = true;
                    session.paused.store(true, Ordering::SeqCst);
                    emit(answer(
                        json!({"type": "status", "level": "info", "message": "Paused"}),
                        id,
                    ));
                }
                None => refuse("not listening", id),
            },
            Ok(Command::Resume(id)) => match turn.as_mut() {
                Some(t) => {
                    t.paused = false;
                    // Quiet while paused doesn't count toward --auto-finish,
                    // nor the moment it takes to reopen the mic and start
                    // talking again.
                    t.voice_ms = segs.now_ms + RESUME_GRACE_MS;
                    session.paused.store(false, Ordering::SeqCst);
                    emit(answer(
                        json!({"type": "status", "level": "info", "message": "Listening"}),
                        id,
                    ));
                }
                None => refuse("not listening", id),
            },
            Ok(Command::Ping(id)) => emit(answer(json!({"type": "pong"}), id)),
            Ok(Command::SetText(text, id)) => {
                // Acknowledged first: partials and finals sent before
                // this line are about the text as it was.
                emit(answer(json!({"type": "text_set"}), id));
                segs.set_text(text);
            }
            Ok(Command::Reset(id)) => {
                emit(answer(json!({"type": "text_set"}), id));
                segs.set_text(String::new());
            }
            Ok(Command::DeleteWord(id)) => {
                if !segs.delete_word() {
                    emit(answer(
                        json!({"type": "status", "level": "info", "message": "Nothing to delete"}),
                        id,
                    ));
                }
            }
            // Whoever started us closed stdin: give them what was said.
            // A file has its own end, so it is left to run out.
            Ok(Command::Eof) if !file => break ("stdin_closed", None),
            Ok(Command::Eof) | Err(_) => {}
        }
        if let Ok(ev) = events.recv_timeout(Duration::from_millis(50)) {
            handle(ev, &session, &mut segs, &mut speaking, turn.as_mut())?;
        }
        // Quiet long enough after words: the turn finishes itself.
        let quiet = turn.as_ref().is_some_and(|t| {
            t.auto_finish_ms.is_some_and(|ms| {
                t.words && !t.paused && !speaking && segs.now_ms.saturating_sub(t.voice_ms) >= ms
            })
        });
        if quiet {
            if ready.standby {
                turn = None;
                finish_turn(
                    &session,
                    &mut segs,
                    &events,
                    &mut speaking,
                    ready.gap_ms,
                    &signalled,
                    "silence",
                    None,
                )?;
            } else {
                break ("silence", None);
            }
        }
    };

    // Finish: no more audio, drain every pass, commit the rest.
    info!(reason, "finishing the jsonl session");
    if speaking {
        emit(json!({"type": "vad_end"}));
    }
    let path = session.path();
    let t = std::thread::scope(|sc| {
        let finishing = sc.spawn(move || session.finish());
        while !finishing.is_finished() {
            if signalled.load(Ordering::SeqCst) {
                session_end("sigterm", None);
                std::process::exit(0);
            }
            // Drained so the passes never wait on a full queue; the
            // finished transcript below has the last word.
            let _ = events.recv_timeout(Duration::from_millis(50));
        }
        finishing
            .join()
            .unwrap_or_else(|_| Err(anyhow::anyhow!("finishing the session panicked")))
    });
    let t = t?;
    let mut rest: Vec<&Paragraph> = t
        .paragraphs
        .iter()
        .filter(|p| !segs.committed.contains(&p.id))
        .collect();
    rest.sort_by_key(|p| p.start_ms);
    for p in rest {
        segs.commit(p);
    }
    if !segs.partial.is_empty() {
        warn!("a partial was left without a final");
    }
    session_end(reason, id);
    Ok(Outcome {
        transcript: t,
        path,
        confirmed: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vox_transcribe::model::{Clip, Stage};

    fn para(id: &str, start_ms: u64, clips: &[(&str, Stage)]) -> Paragraph {
        let clips: Vec<Clip> = clips
            .iter()
            .enumerate()
            .map(|(i, (text, stage))| Clip {
                id: format!("{id}-{i}"),
                start_ms: start_ms + i as u64 * 1000,
                duration_ms: Some(900),
                text: text.to_string(),
                stage: *stage,
                // Finished words 200 ms apart, as pass 2 would time them.
                words: if *stage == Stage::Partial {
                    Vec::new()
                } else {
                    text.split_whitespace()
                        .enumerate()
                        .map(|(j, w)| vox_transcribe::Word {
                            text: w.into(),
                            start_ms: start_ms + i as u64 * 1000 + j as u64 * 200,
                            end_ms: start_ms + i as u64 * 1000 + j as u64 * 200 + 180,
                        })
                        .collect()
                },
                speaker: None,
            })
            .collect();
        let text = clips
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        Paragraph {
            id: id.into(),
            start_ms,
            end_ms: start_ms + 1000,
            text,
            clips,
            words: Vec::new(),
            speaker: None,
            closed: false,
            hardened: false,
            pass3_inflight: false,
            pass4: None,
            translation: None,
        }
    }

    #[test]
    fn pass1_guesses_show_in_lowercase() {
        let p = para(
            "p1",
            0,
            &[
                ("Could you inspect", Stage::Final),
                ("THIS PROJECT", Stage::Partial),
            ],
        );
        assert_eq!(
            Segments::new(Pass1::Text).text(&p),
            "Could you inspect this project"
        );
        assert_eq!(Segments::new(Pass1::Hidden).text(&p), "Could you inspect");
        assert_eq!(
            Segments::new(Pass1::Ellipsis).text(&p),
            "Could you inspect ……"
        );
    }

    #[test]
    fn paragraphs_commit_in_order() {
        let mut s = Segments::new(Pass1::Text);
        s.update(para("a", 0, &[("One.", Stage::Final)]));
        let mut b = para("b", 5000, &[("Two.", Stage::Final)]);
        b.hardened = true;
        s.update(b);
        s.flush();
        // b hardened first, but waits for a.
        assert!(s.committed.is_empty());
        assert_eq!(s.partial, "One. Two.");
        let mut a = para("a", 0, &[("One!", Stage::Final)]);
        a.hardened = true;
        s.update(a);
        s.flush();
        assert!(s.committed.contains("a") && s.committed.contains("b"));
        assert!(s.open.is_empty());
        assert_eq!(s.partial, "");
        assert_eq!(s.committed_text, "One! Two. ");
        // Late updates to a committed paragraph are ignored.
        s.update(para("a", 0, &[("changed", Stage::Final)]));
        assert!(s.open.is_empty());
    }

    #[test]
    fn deleted_words_stay_deleted_when_re_decoded() {
        let mut s = Segments::new(Pass1::Text);
        s.update(para("a", 0, &[("Could you inspect this", Stage::Final)]));
        s.flush();
        assert!(s.delete_word());
        assert_eq!(s.partial, "Could you inspect");
        // Pass 3 rewrites the clip; the cut holds by time.
        s.update(para("a", 0, &[("Could you inspect these", Stage::Revised)]));
        s.flush();
        assert_eq!(s.partial, "Could you inspect");
    }

    #[test]
    fn pass1_words_are_deleted_and_then_cut_by_time() {
        let mut s = Segments::new(Pass1::Text);
        s.now_ms = 300;
        s.update(para("a", 0, &[("COULD YOU", Stage::Partial)]));
        // "inspect" is said 400-580 ms, "this" 600-780 (see `para`).
        s.now_ms = 650;
        s.update(para("a", 0, &[("COULD YOU INSPECT", Stage::Partial)]));
        s.flush();
        assert!(s.delete_word());
        assert_eq!(s.partial, "could you");
        // More pass-1 words keep the deleted one out.
        s.now_ms = 850;
        s.update(para("a", 0, &[("COULD YOU INSPECT THIS", Stage::Partial)]));
        s.flush();
        assert_eq!(s.partial, "could you this");
        // Pass 2 times the clip: the last word said by 650 ms goes.
        s.update(para("a", 0, &[("Could you inspect this", Stage::Final)]));
        s.flush();
        assert_eq!(s.partial, "Could you this");
        assert!(s.hidden.is_empty());
    }

    #[test]
    fn set_text_takes_what_was_heard_and_new_speech_follows_it() {
        let mut s = Segments::new(Pass1::Text);
        s.now_ms = 650;
        s.update(para("a", 0, &[("COULD YOU INSPECT", Stage::Partial)]));
        s.flush();
        // Edited by hand, ending without a space.
        s.set_text("Could we inspect".into());
        assert_eq!(s.partial, "");
        s.now_ms = 850;
        s.update(para("a", 0, &[("COULD YOU INSPECT THIS", Stage::Partial)]));
        s.flush();
        assert_eq!(s.partial, " this");
        // Pass 2: words said before the edit stay out.
        let mut a = para("a", 0, &[("Could you inspect this", Stage::Final)]);
        a.hardened = true;
        s.update(a);
        s.flush();
        assert_eq!(s.committed_text, "Could we inspect this ");
        assert!(s.delete_word());
        assert!(s.delete_word());
        assert_eq!(s.committed_text, "Could we ");
    }

    #[test]
    fn with_no_partial_the_committed_text_is_retracted() {
        let mut s = Segments::new(Pass1::Text);
        let mut a = para("a", 0, &[("Hello there.", Stage::Final)]);
        a.hardened = true;
        s.update(a);
        s.flush();
        assert!(s.delete_word());
        assert_eq!(s.committed_text, "Hello ");
        assert!(s.delete_word());
        assert_eq!(s.committed_text, "");
        assert!(!s.delete_word());
    }
}
