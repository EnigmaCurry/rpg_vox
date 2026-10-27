//! `--jsonl`: live dictation for another program, which starts scribe,
//! reads transcript events on stdout and sends commands on stdin, one
//! JSON object per line each way (the protocol is in SCRIBE_REFERENCE.md).
//!
//! Each paragraph is one segment. While any pass may still change it,
//! its text is part of the `partial`; once it hardens it is sent as a
//! `final` and never touched again. Paragraphs are committed in order,
//! so a later one that hardens first waits in the partial for the one
//! before it.

use std::collections::HashSet;
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

/// What `ready` reports.
pub struct Ready {
    pub model: String,
    pub sample_rate_hz: u32,
}

/// A line read from stdin.
enum Command {
    Finish(Option<Value>),
    Cancel(Option<Value>),
    Stop(Option<Value>),
    Ping(Option<Value>),
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
                    Some("finish") => Command::Finish(id),
                    Some("cancel") => Command::Cancel(id),
                    Some("stop") => Command::Stop(id),
                    Some("ping") => Command::Ping(id),
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

/// `session_end`, with the command's `request_id` if it had one.
fn session_end(reason: &str, id: Option<Value>) {
    let mut e = json!({"type": "session_end", "reason": reason});
    if let Some(id) = id {
        e["request_id"] = id;
    }
    emit(e);
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
#[derive(Default)]
struct Segments {
    open: Vec<Paragraph>,
    committed: HashSet<String>,
    /// The partial last sent, so an unchanged one isn't repeated.
    partial: String,
}

impl Segments {
    fn update(&mut self, p: Paragraph) {
        if self.committed.contains(&p.id) {
            return;
        }
        match self.open.iter_mut().find(|o| o.id == p.id) {
            Some(o) => *o = p,
            None => {
                let at = self.open.partition_point(|o| o.start_ms <= p.start_ms);
                self.open.insert(at, p);
            }
        }
    }

    fn remove(&mut self, id: &str) {
        self.open.retain(|o| o.id != id);
    }

    /// Commit the hardened paragraphs at the front, then send what is
    /// left as the partial if it changed.
    fn flush(&mut self, pass1: Pass1) {
        while self.open.first().is_some_and(|p| p.hardened) {
            let p = self.open.remove(0);
            self.commit(&p);
        }
        let partial = self
            .open
            .iter()
            .map(|p| shown(p, pass1))
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if partial != self.partial {
            emit(
                json!({"type": "partial", "text": partial, "segment_id": self.open.first().map(|p| p.id.clone())}),
            );
            self.partial = partial;
        }
    }

    fn commit(&mut self, p: &Paragraph) {
        if !self.committed.insert(p.id.clone()) {
            return;
        }
        let text = p.text.trim();
        if text.is_empty() {
            return;
        }
        // The trailing space keeps it apart from the next segment.
        emit(json!({"type": "final", "text": format!("{text} "), "segment_id": p.id}));
        // A final clears the partial on the other side.
        self.partial.clear();
    }
}

/// A paragraph as the partial shows it: finished clips as they are,
/// pass 1's ALL CAPS guesses in lowercase (or as --pass1 says).
fn shown(p: &Paragraph, pass1: Pass1) -> String {
    p.clips
        .iter()
        .flat_map(|c| {
            if c.is_partial() {
                pass1.words(&c.text.to_lowercase())
            } else {
                c.text.split_whitespace().map(str::to_string).collect()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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
    }));
    let session_id = format!("voice-{}", chrono::Utc::now().to_rfc3339());
    emit(json!({"type": "session_start", "session_id": session_id}));
    let file = session.input_ms.is_some();
    emit(
        json!({"type": "status", "level": "info", "message": if file { "Transcribing" } else { "Listening" }}),
    );

    let events = session.engine.events().clone();
    let mut segs = Segments::default();
    let mut speaking = false;
    // Why the session is being finished, and the request that asked.
    let (reason, id) = loop {
        if signalled.load(Ordering::SeqCst) {
            end_now(&session, "sigterm", None);
        }
        if session.source_done.load(Ordering::SeqCst) {
            break ("end_of_input", None);
        }
        match commands.try_recv() {
            Ok(Command::Finish(id)) => break ("finished", id),
            Ok(Command::Cancel(id)) => end_now(&session, "cancelled", id),
            Ok(Command::Stop(id)) => end_now(&session, "stop_requested", id),
            Ok(Command::Ping(id)) => {
                let mut e = json!({"type": "pong"});
                if let Some(id) = id {
                    e["request_id"] = id;
                }
                emit(e);
            }
            // Whoever started us closed stdin: give them what was said.
            // A file has its own end, so it is left to run out.
            Ok(Command::Eof) if !file => break ("stdin_closed", None),
            Ok(Command::Eof) | Err(_) => {}
        }
        match events.recv_timeout(Duration::from_millis(50)) {
            Ok(Event::Paragraph { paragraph, .. }) => {
                if paragraph.hardened {
                    session.write(&paragraph)?;
                }
                segs.update(paragraph);
                segs.flush(session.pass1);
            }
            Ok(Event::ParagraphRemoved { id }) => {
                segs.remove(&id);
                segs.flush(session.pass1);
            }
            Ok(Event::Level { speaking: now, .. }) if now != speaking => {
                speaking = now;
                emit(json!({"type": if now { "vad_start" } else { "vad_end" }}));
            }
            _ => {}
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
                words: Vec::new(),
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
        assert_eq!(shown(&p, Pass1::Text), "Could you inspect this project");
        assert_eq!(shown(&p, Pass1::Hidden), "Could you inspect");
        assert_eq!(shown(&p, Pass1::Ellipsis), "Could you inspect ……");
    }

    #[test]
    fn paragraphs_commit_in_order() {
        let mut s = Segments::default();
        s.update(para("a", 0, &[("One.", Stage::Final)]));
        let mut b = para("b", 5000, &[("Two.", Stage::Final)]);
        b.hardened = true;
        s.update(b);
        s.flush(Pass1::Text);
        // b hardened first, but waits for a.
        assert!(s.committed.is_empty());
        assert_eq!(s.partial, "One. Two.");
        let mut a = para("a", 0, &[("One!", Stage::Final)]);
        a.hardened = true;
        s.update(a);
        s.flush(Pass1::Text);
        assert!(s.committed.contains("a") && s.committed.contains("b"));
        assert!(s.open.is_empty());
        assert_eq!(s.partial, "");
        // Late updates to a committed paragraph are ignored.
        s.update(para("a", 0, &[("changed", Stage::Final)]));
        assert!(s.open.is_empty());
    }
}
