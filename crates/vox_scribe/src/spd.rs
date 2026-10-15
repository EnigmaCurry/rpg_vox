//! Speech-dispatcher as a voice, spoken to over SSIP.
//!
//! The desktop's speech daemon reads replies aloud in whatever voices it
//! has been set up with (espeak-ng, pocket-tts, pico, a paid
//! synthesizer), so a machine where speech already works needs no model
//! of scribe's own.
//!
//! SSIP is a line protocol on a unix socket. A reply is a status code
//! and, where there is a payload, `NNN-line` continuations before the
//! final `NNN text`:
//!
//! ```text
//! > SPEAK
//! < 230 OK RECEIVING DATA
//! > <mark name="0"/>Hello <mark name="1"/>there.
//! > .
//! < 225-37
//! < 225 OK MESSAGE QUEUED
//! ```
//!
//! While that message is spoken the daemon volunteers 7xx events on the
//! same socket — `701 BEGIN`, a `700 INDEX MARK` for each `<mark>` the
//! voice reaches, `702 END` — which is where the word being heard comes
//! from. Modules that report no marks (pocket-tts among them) leave the
//! word to be guessed from how long the message has been sounding.
//!
//! No audio comes back: the daemon plays it. That is why `--chat` can
//! speak this way but reading a transcript aloud (`--play NAME` with no
//! NAME.opus) cannot — that player seeks and stretches the samples
//! themselves, which only [`crate::voice::Kokoro`] hands over.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context as _, Result};
use crossbeam_channel::{Receiver, Sender};
use tracing::warn;

use crate::tts::{one_less, Choice};

/// How long to wait for a reply before giving up on the daemon.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// Guess at how long a character takes to say, until a message has been
/// spoken and timed: 60 ms is about 170 words a minute.
const MS_PER_CHAR: f64 = 60.0;
/// How much of that guess a freshly timed message replaces.
const LEARN: f64 = 0.3;
/// How often the word being heard is worked out again.
const TICK: Duration = Duration::from_millis(25);

/// Where the daemon listens: `SPEECHD_ADDRESS` as its own clients read
/// it, else `$XDG_RUNTIME_DIR/speech-dispatcher/speechd.sock`.
pub fn socket_path() -> Result<PathBuf> {
    if let Some(addr) = std::env::var_os("SPEECHD_ADDRESS") {
        return address_path(&addr.to_string_lossy());
    }
    let run = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => PathBuf::from(dir),
        // Where the session's runtime directory is on Linux anyway.
        None => PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })),
    };
    Ok(run.join("speech-dispatcher").join("speechd.sock"))
}

/// `SPEECHD_ADDRESS` as the daemon's own clients read it. Only the unix
/// socket of the two forms it allows: a remote daemon would speak on the
/// machine it runs on, not this one.
fn address_path(addr: &str) -> Result<PathBuf> {
    match addr.split_once(':') {
        Some(("unix_socket", path)) => Ok(PathBuf::from(path)),
        _ => bail!("SPEECHD_ADDRESS={addr}: scribe speaks over unix_socket:PATH only"),
    }
}

/// Whether a speech-dispatcher is listening and willing to talk. Only
/// asks: a daemon that is not running is left alone rather than spawned,
/// so `--tts auto` never starts one as a side effect.
pub fn detect() -> bool {
    match Conn::open("detect") {
        Ok(mut conn) => {
            conn.quit();
            true
        }
        Err(_) => false,
    }
}

/// The module in use and the voices it offers, for `scribe voices`.
pub fn voices() -> Result<(String, Vec<Choice>)> {
    let mut conn = Conn::open("voices")?;
    let module = conn.get("OUTPUT_MODULE")?;
    let language = conn.get("LANGUAGE")?;
    let offered = choices_from(&conn.voices()?, &module, &language);
    conn.quit();
    Ok((module, offered))
}

/// A reply to one command.
#[derive(Debug, Clone, PartialEq)]
struct Reply {
    code: u16,
    /// The final line after its code, e.g. "OK MESSAGE QUEUED".
    text: String,
    /// The `NNN-` lines before it, codes stripped.
    lines: Vec<String>,
}

/// What the daemon says about a message of ours as it is spoken.
#[derive(Debug, Clone, PartialEq)]
enum Event {
    Begin(u64),
    End(u64),
    /// Speech reached a `<mark>`: the message, and the mark's name.
    Mark(u64, String),
    Cancelled(u64),
}

/// One of the daemon's voices, as `LIST SYNTHESIS_VOICES` gives it.
#[derive(Debug, Clone, PartialEq)]
struct SynthesisVoice {
    name: String,
    language: String,
    variant: String,
}

/// `NNN-text` or `NNN text` split into the code, whether more lines
/// follow, and the text.
fn parse_line(line: &str) -> Option<(u16, bool, &str)> {
    if line.len() < 4 {
        return None;
    }
    let code: u16 = line[..3].parse().ok()?;
    let more = match line.as_bytes()[3] {
        b'-' => true,
        b' ' => false,
        _ => return None,
    };
    Some((code, more, &line[4..]))
}

/// A 7xx block as an event; `None` for the ones scribe drives itself
/// (pause and resume) and any the daemon grows later.
fn parse_event(code: u16, lines: &[String]) -> Option<Event> {
    let id = lines.first()?.trim().parse().ok()?;
    match code {
        700 => Some(Event::Mark(id, lines.get(2)?.trim().to_string())),
        701 => Some(Event::Begin(id)),
        702 => Some(Event::End(id)),
        703 => Some(Event::Cancelled(id)),
        _ => None,
    }
}

/// One listed voice: name, language and variant, tab separated.
fn parse_voice(line: &str) -> Option<SynthesisVoice> {
    let mut fields = line.split('\t');
    let name = fields.next()?.trim();
    if name.is_empty() {
        return None;
    }
    Some(SynthesisVoice {
        name: name.to_string(),
        language: fields.next().unwrap_or("").trim().to_string(),
        variant: fields.next().unwrap_or("").trim().to_string(),
    })
}

/// Splits what the daemon sends into replies to our commands and the
/// events it volunteers while speaking. Both arrive on the one socket,
/// interleaved, so this is its only reader.
fn read_lines(sock: UnixStream, replies: &Sender<Reply>, events: &Sender<Event>) {
    let mut lines: Vec<String> = Vec::new();
    for line in BufReader::new(sock).lines() {
        let Ok(line) = line else { return };
        let line = line.trim_end_matches('\r');
        let Some((code, more, text)) = parse_line(line) else {
            warn!("speech-dispatcher said {line:?}");
            continue;
        };
        if more {
            lines.push(text.to_string());
            continue;
        }
        let lines = std::mem::take(&mut lines);
        // Events share the socket with replies but answer nothing, so
        // they go their own way rather than being mistaken for one.
        let sent = if (700..800).contains(&code) {
            match parse_event(code, &lines) {
                Some(event) => events.send(event).is_ok(),
                None => true,
            }
        } else {
            replies
                .send(Reply {
                    code,
                    text: text.to_string(),
                    lines,
                })
                .is_ok()
        };
        if !sent {
            return;
        }
    }
}

/// One SSIP connection, written to from whoever holds it and read by a
/// thread of its own.
struct Conn {
    sock: UnixStream,
    replies: Receiver<Reply>,
    events: Receiver<Event>,
}

impl Conn {
    /// Connect and say who we are; `role` tells our connections apart in
    /// the daemon's log.
    fn open(role: &str) -> Result<Self> {
        let path = socket_path()?;
        let sock = UnixStream::connect(&path).with_context(|| {
            format!(
                "connect to speech-dispatcher at {} (is it running? try `spd-say hello`)",
                path.display()
            )
        })?;
        let (reply_tx, replies) = crossbeam_channel::unbounded();
        let (event_tx, events) = crossbeam_channel::unbounded();
        let reader = sock
            .try_clone()
            .context("clone the speech-dispatcher socket")?;
        std::thread::Builder::new()
            .name("spd-read".into())
            .spawn(move || read_lines(reader, &reply_tx, &event_tx))?;
        let mut conn = Self {
            sock,
            replies,
            events,
        };
        let user = std::env::var("USER").unwrap_or_else(|_| "scribe".into());
        // SSIP wants this first, as user:client:connection.
        conn.cmd(&format!("SET SELF CLIENT_NAME {user}:scribe:{role}"))?;
        Ok(conn)
    }

    fn write_line(&mut self, line: &str) -> Result<()> {
        self.sock
            .write_all(format!("{line}\r\n").as_bytes())
            .context("write to speech-dispatcher")?;
        self.sock.flush().context("write to speech-dispatcher")?;
        Ok(())
    }

    fn reply(&mut self) -> Result<Reply> {
        self.replies
            .recv_timeout(REPLY_TIMEOUT)
            .context("speech-dispatcher stopped answering")
    }

    /// Send one command and wait for its reply; a refusal is an error.
    fn cmd(&mut self, line: &str) -> Result<Reply> {
        self.write_line(line)?;
        let reply = self.reply()?;
        if reply.code >= 300 {
            bail!(
                "speech-dispatcher refused {line:?}: {} {}",
                reply.code,
                reply.text
            );
        }
        Ok(reply)
    }

    fn set(&mut self, what: &str, value: &str) -> Result<()> {
        self.cmd(&format!("SET SELF {what} {value}")).map(|_| ())
    }

    /// A setting of this connection's, e.g. `GET LANGUAGE`.
    fn get(&mut self, what: &str) -> Result<String> {
        let reply = self.cmd(&format!("GET {what}"))?;
        reply
            .lines
            .first()
            .map(|l| l.trim().to_string())
            .ok_or_else(|| anyhow!("speech-dispatcher answered GET {what} with nothing"))
    }

    /// Hand one SSML message over to be spoken; the id it answers with
    /// names that message in the events that follow.
    fn speak(&mut self, ssml: &str) -> Result<u64> {
        self.cmd("SPEAK")?;
        for line in ssml.lines() {
            // A line of one dot ends the data, so a line that starts
            // with a dot is sent with it doubled, as the protocol asks.
            if line.starts_with('.') {
                self.write_line(&format!(".{line}"))?;
            } else {
                self.write_line(line)?;
            }
        }
        let queued = self.cmd(".")?;
        queued
            .lines
            .first()
            .and_then(|l| l.trim().parse().ok())
            .ok_or_else(|| anyhow!("speech-dispatcher queued a message without an id"))
    }

    /// Drop what is being said and everything queued behind it.
    fn cancel(&mut self) -> Result<()> {
        self.cmd("CANCEL SELF").map(|_| ())
    }

    fn pause(&mut self) -> Result<()> {
        self.cmd("PAUSE SELF").map(|_| ())
    }

    fn resume(&mut self) -> Result<()> {
        self.cmd("RESUME SELF").map(|_| ())
    }

    fn voices(&mut self) -> Result<Vec<SynthesisVoice>> {
        let reply = self.cmd("LIST SYNTHESIS_VOICES")?;
        Ok(reply.lines.iter().filter_map(|l| parse_voice(l)).collect())
    }

    /// Say goodbye, so the daemon forgets us rather than waiting for the
    /// socket to close.
    fn quit(&mut self) {
        let _ = self.write_line("QUIT");
    }
}

/// `module:voice`, or a voice of the module already in use.
fn choose(conn: &mut Conn, name: &str) -> Result<()> {
    let (module, voice) = match name.split_once(':') {
        Some((module, voice)) => (Some(module), voice),
        None => (None, name),
    };
    if let Some(module) = module {
        conn.set("OUTPUT_MODULE", module)?;
    }
    conn.set("SYNTHESIS_VOICE", voice)
}

/// The voices `v` offers: what the module in use has for the language in
/// use ("en" covering en-us, en-gb, …), one entry per name. Everything it
/// has, if that leaves nothing — a daemon set to a language its module
/// doesn't list still speaks.
fn choices_from(voices: &[SynthesisVoice], module: &str, language: &str) -> Vec<Choice> {
    let family = |lang: &str| lang.split('-').next().unwrap_or(lang).to_string();
    let wanted = family(language);
    let mut pick: Vec<&SynthesisVoice> = voices
        .iter()
        .filter(|v| family(&v.language) == wanted)
        .collect();
    if pick.is_empty() {
        pick = voices.iter().collect();
    }
    let mut seen: HashSet<&str> = HashSet::new();
    pick.into_iter()
        .filter(|v| seen.insert(&v.name))
        .map(|v| Choice {
            // Qualified, so reopening a chat brings back the module too.
            name: format!("{module}:{}", v.name),
            label: label(v),
        })
        .collect()
}

/// The voice "vera", "en", "FEMALE3" → "vera · en female3".
fn label(voice: &SynthesisVoice) -> String {
    let variant = voice.variant.to_lowercase();
    let variant = match variant.as_str() {
        "" | "none" => String::new(),
        v => format!(" {v}"),
    };
    format!("{} · {}{variant}", voice.name, voice.language)
}

/// `text` as SSML with a `<mark>` before every word, so the daemon
/// reports where the voice has got to, word by word.
fn marked_ssml(words: &[&str]) -> String {
    let mut out = String::new();
    for (i, word) in words.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&format!("<mark name=\"{i}\"/>"));
        for c in word.chars() {
            match c {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                c => out.push(c),
            }
        }
    }
    out
}

/// Something for the speaking thread to do. Pause and stop travel with
/// the text so they keep their order with it.
enum Job {
    Say {
        generation: u64,
        key: i64,
        text: String,
    },
    Voice(String),
    Paused(bool),
    Stop,
}

#[derive(Default)]
struct Shared {
    /// Bumped by [`Spd::stop`]; work from an older generation is dropped.
    generation: AtomicU64,
    /// Messages handed to [`Spd::say`] and not yet finished.
    pending: AtomicUsize,
    paused: AtomicBool,
    /// (key, index of the word being heard among all the words said
    /// under that key).
    heard: Mutex<Option<(i64, usize)>>,
}

/// One message the daemon has been given.
struct Utterance {
    id: u64,
    key: i64,
    /// Index of its first word among the words said under `key`.
    first_word: usize,
    /// Each word's length (and the space after it), to spread the guess
    /// when no index marks come.
    weights: Vec<u64>,
    /// When the daemon said it had begun.
    started: Option<Instant>,
    paused_since: Option<Instant>,
    paused_for: Duration,
    /// The word the last index mark reported.
    marked: Option<usize>,
}

impl Utterance {
    /// How long it has been sounding, pauses taken out.
    fn sounding_for(&self, started: Instant) -> Duration {
        self.paused_since
            .unwrap_or_else(Instant::now)
            .saturating_duration_since(started)
            .saturating_sub(self.paused_for)
    }

    /// The word being heard: the last index mark, or where the clock
    /// says the voice should be by now.
    fn word(&self, ms_per_char: f64) -> Option<usize> {
        let started = self.started?;
        if let Some(i) = self.marked {
            return Some(i.min(self.weights.len().saturating_sub(1)));
        }
        let elapsed = self.sounding_for(started).as_millis() as f64;
        let mut until = 0.0;
        for (i, weight) in self.weights.iter().enumerate() {
            until += *weight as f64 * ms_per_char;
            if elapsed < until {
                return Some(i);
            }
        }
        // Never past the last word: the END event says when it is over.
        Some(self.weights.len().saturating_sub(1))
    }

    fn set_paused(&mut self, paused: bool) {
        match (paused, self.paused_since) {
            (true, None) => self.paused_since = Some(Instant::now()),
            (false, Some(since)) => {
                self.paused_for += since.elapsed();
                self.paused_since = None;
            }
            _ => {}
        }
    }
}

/// Speech-dispatcher reading text as it arrives, with the same shape as
/// [`crate::voice::Kokoro`]: text in, the word being heard out.
pub struct Spd {
    jobs: Sender<Job>,
    shared: Arc<Shared>,
    choices: Vec<Choice>,
    /// Voice in use when the chat opens: the one asked for, else the
    /// daemon's own, which it will not name.
    name: String,
}

impl Spd {
    /// Connect, read the voices on offer, and start speaking. `voice` is
    /// a `--voices` name (`vera`, or `espeak-ng:English (America)+Alex`)
    /// to start in; without one the daemon's own voice is left alone.
    pub fn open(voice: Option<&str>) -> Result<Self> {
        let mut conn = Conn::open("chat")?;
        conn.set("SSML_MODE", "ON")?;
        conn.set("NOTIFICATION ALL", "ON")?;
        // Queued, not interrupting: a reply arrives a sentence at a
        // time, and under the "text" priority each new sentence would
        // cancel the one being spoken, leaving only the last of them.
        conn.set("PRIORITY", "message")?;
        // A module named along with the voice has to be set first, or
        // the voices listed would be the wrong module's.
        if let Some((module, _)) = voice.and_then(|v| v.split_once(':')) {
            conn.set("OUTPUT_MODULE", module)?;
        }
        let module = conn.get("OUTPUT_MODULE")?;
        let language = conn.get("LANGUAGE")?;
        let offered = conn.voices()?;
        if let Some(asked) = voice {
            // Checked here because the daemon takes any name it is
            // given and quietly goes on in the voice it had.
            let name = asked.split_once(':').map_or(asked, |(_, v)| v);
            if !offered.iter().any(|v| v.name == name) {
                bail!(
                    "speech-dispatcher's {module} module has no voice {name:?}; \
                     `scribe voices` lists the ones it has"
                );
            }
            conn.set("SYNTHESIS_VOICE", name)?;
        }
        let choices = choices_from(&offered, &module, &language);
        let name = match voice {
            Some(voice) if voice.contains(':') => voice.to_string(),
            Some(voice) => format!("{module}:{voice}"),
            None => module,
        };
        let shared = Arc::new(Shared::default());
        let (jobs, job_rx) = crossbeam_channel::unbounded();
        {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("spd-speak".into())
                .spawn(move || speak_jobs(conn, &shared, job_rx))?;
        }
        Ok(Self {
            jobs,
            shared,
            choices,
            name,
        })
    }

    /// Queue `text` to be spoken after anything already queued. Its
    /// words are counted on from earlier text with the same `key`.
    pub fn say(&self, key: i64, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let generation = self.shared.generation.load(Ordering::SeqCst);
        self.shared.pending.fetch_add(1, Ordering::SeqCst);
        let _ = self.jobs.send(Job::Say {
            generation,
            key,
            text: text.to_string(),
        });
    }

    /// The word being heard now: (key, its index under that key).
    pub fn heard(&self) -> Option<(i64, usize)> {
        *self.shared.heard.lock().expect("heard lock")
    }

    /// Something is queued, being made, or playing (paused or not).
    pub fn busy(&self) -> bool {
        self.shared.pending.load(Ordering::SeqCst) > 0
    }

    pub fn set_paused(&self, paused: bool) {
        if self.shared.paused.swap(paused, Ordering::SeqCst) != paused {
            let _ = self.jobs.send(Job::Paused(paused));
        }
    }

    /// Speak in `name` from the next message on; false if no such voice.
    pub fn set_voice(&self, name: &str) -> bool {
        if !self.choices.iter().any(|c| c.name == name) {
            return false;
        }
        let _ = self.jobs.send(Job::Voice(name.to_string()));
        true
    }

    /// Drop everything not yet heard.
    pub fn stop(&self) {
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        self.shared.pending.store(0, Ordering::SeqCst);
        self.shared.paused.store(false, Ordering::SeqCst);
        // Cleared here rather than left to the speaking thread, so a
        // word said before the stop is never reported after it.
        *self.shared.heard.lock().expect("heard lock") = None;
        let _ = self.jobs.send(Job::Stop);
    }

    pub fn choices(&self) -> &[Choice] {
        &self.choices
    }

    /// The voice in use when the chat opened.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Hands text to the daemon one message at a time and follows the events
/// it sends back, so [`Spd::heard`] keeps up with the voice.
fn speak_jobs(mut conn: Conn, shared: &Shared, jobs: Receiver<Job>) {
    let events = conn.events.clone();
    // Messages given to the daemon and not yet finished, oldest first.
    let mut queue: VecDeque<Utterance> = VecDeque::new();
    // Words said so far under each key.
    let mut counts: HashMap<i64, usize> = HashMap::new();
    let mut counted_for = shared.generation.load(Ordering::SeqCst);
    // Learned from the messages that have been spoken, for the modules
    // that report no index marks.
    let mut ms_per_char = MS_PER_CHAR;
    loop {
        crossbeam_channel::select! {
            recv(jobs) -> job => match job {
                // The chat is gone: stop talking rather than read on.
                Err(_) => {
                    let _ = conn.cancel();
                    conn.quit();
                    return;
                }
                Ok(Job::Say { generation, key, text }) => {
                    if generation != shared.generation.load(Ordering::SeqCst) {
                        continue;
                    }
                    // After a stop, a key's words count from 0 again.
                    if generation != counted_for {
                        counts.clear();
                        counted_for = generation;
                    }
                    let words: Vec<&str> = text.split_whitespace().collect();
                    let first_word = *counts.get(&key).unwrap_or(&0);
                    counts.insert(key, first_word + words.len());
                    match conn.speak(&marked_ssml(&words)) {
                        Ok(id) => queue.push_back(Utterance {
                            id,
                            key,
                            first_word,
                            weights: words
                                .iter()
                                .map(|w| w.chars().count() as u64 + 1)
                                .collect(),
                            started: None,
                            paused_since: None,
                            paused_for: Duration::ZERO,
                            marked: None,
                        }),
                        Err(e) => {
                            warn!("speaking {text:?} failed: {e:#}");
                            one_less(&shared.pending);
                        }
                    }
                }
                Ok(Job::Voice(name)) => {
                    if let Err(e) = choose(&mut conn, &name) {
                        warn!("speaking in {name:?} failed: {e:#}");
                    }
                }
                Ok(Job::Paused(paused)) => {
                    let asked = if paused { conn.pause() } else { conn.resume() };
                    if let Err(e) = asked {
                        warn!("pausing speech-dispatcher failed: {e:#}");
                    }
                    if let Some(u) = queue.front_mut() {
                        u.set_paused(paused);
                    }
                }
                Ok(Job::Stop) => {
                    if let Err(e) = conn.cancel() {
                        warn!("stopping speech-dispatcher failed: {e:#}");
                    }
                    queue.clear();
                }
            },
            recv(events) -> event => match event {
                // The daemon hung up; nothing more will be spoken.
                Err(_) => return,
                Ok(Event::Begin(id)) => {
                    if let Some(u) = queue.iter_mut().find(|u| u.id == id) {
                        u.started = Some(Instant::now());
                        u.set_paused(shared.paused.load(Ordering::SeqCst));
                    }
                }
                Ok(Event::Mark(id, name)) => {
                    if let (Some(u), Ok(i)) = (
                        queue.iter_mut().find(|u| u.id == id),
                        name.trim().parse::<usize>(),
                    ) {
                        u.marked = Some(i);
                    }
                }
                Ok(Event::End(id)) | Ok(Event::Cancelled(id)) => {
                    if let Some(at) = queue.iter().position(|u| u.id == id) {
                        let done = queue.remove(at).expect("the message just found");
                        // What that message really took, for the next
                        // guess — only worth having where the module
                        // leaves the words to be guessed.
                        if done.marked.is_none() {
                            if let Some(started) = done.started {
                                let chars: u64 = done.weights.iter().sum();
                                let ms = done.sounding_for(started).as_millis() as f64;
                                if chars > 0 && ms > 0.0 {
                                    let measured = ms / chars as f64;
                                    ms_per_char = ms_per_char * (1.0 - LEARN) + measured * LEARN;
                                }
                            }
                        }
                        one_less(&shared.pending);
                    }
                }
            },
            default(TICK) => {}
        }
        let heard = queue
            .front()
            .and_then(|u| u.word(ms_per_char).map(|i| (u.key, u.first_word + i)));
        *shared.heard.lock().expect("heard lock") = heard;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_replies_from_events() {
        let (reply_tx, replies) = crossbeam_channel::unbounded();
        let (event_tx, events) = crossbeam_channel::unbounded();
        let (a, b) = UnixStream::pair().unwrap();
        let reader = std::thread::spawn(move || read_lines(b, &reply_tx, &event_tx));
        let mut a = a;
        a.write_all(
            b"225-37\r\n225 OK MESSAGE QUEUED\r\n\
              701-37\r\n701-4\r\n701 BEGIN\r\n\
              700-37\r\n700-4\r\n700-2\r\n700 INDEX MARK\r\n\
              704-37\r\n704-4\r\n704 PAUSED\r\n\
              702-37\r\n702-4\r\n702 END\r\n",
        )
        .unwrap();
        drop(a);
        reader.join().unwrap();
        assert_eq!(
            replies.try_iter().collect::<Vec<_>>(),
            vec![Reply {
                code: 225,
                text: "OK MESSAGE QUEUED".into(),
                lines: vec!["37".into()],
            }]
        );
        // PAUSED is scribe's own doing, so it is not passed on.
        assert_eq!(
            events.try_iter().collect::<Vec<_>>(),
            vec![
                Event::Begin(37),
                Event::Mark(37, "2".into()),
                Event::End(37)
            ]
        );
    }

    #[test]
    fn marks_every_word_and_escapes_the_text() {
        assert_eq!(
            marked_ssml(&["Tom", "&", "<b>"]),
            "<mark name=\"0\"/>Tom <mark name=\"1\"/>&amp; <mark name=\"2\"/>&lt;b&gt;"
        );
    }

    #[test]
    fn reads_the_voice_list() {
        assert_eq!(
            parse_voice("vera\ten\tFEMALE3"),
            Some(SynthesisVoice {
                name: "vera".into(),
                language: "en".into(),
                variant: "FEMALE3".into(),
            })
        );
        assert_eq!(parse_voice(""), None);
        assert_eq!(
            label(&parse_voice("English (America)+Alex\ten-us\tAlex").unwrap()),
            "English (America)+Alex · en-us alex"
        );
        assert_eq!(
            label(&parse_voice("Afrikaans\taf\tnone").unwrap()),
            "Afrikaans · af"
        );
    }

    #[test]
    fn offers_the_language_in_use_once_per_name() {
        let voices: Vec<SynthesisVoice> = [
            "alba\ten\tFEMALE1",
            "alba\ten-gb\tFEMALE1",
            "jean\tfr\tMALE1",
        ]
        .iter()
        .map(|l| parse_voice(l).unwrap())
        .collect();
        assert_eq!(
            choices_from(&voices, "pocket-tts", "en-us"),
            vec![Choice {
                name: "pocket-tts:alba".into(),
                label: "alba · en female1".into(),
            }]
        );
        // A language the module doesn't list leaves all of them on offer
        // rather than none.
        assert_eq!(choices_from(&voices, "pocket-tts", "de").len(), 2);
    }

    #[test]
    fn guesses_the_word_from_the_clock_until_a_mark_arrives() {
        let mut u = Utterance {
            id: 1,
            key: 7,
            first_word: 3,
            // "ab" and "cde": 3 and 4 characters' worth.
            weights: vec![3, 4],
            started: Some(Instant::now() - Duration::from_millis(100)),
            paused_since: None,
            paused_for: Duration::ZERO,
            marked: None,
        };
        // 100 ms in, at 50 ms a character, the first word is still going.
        assert_eq!(u.word(50.0), Some(0));
        // At 10 ms a character both are long over; the last one holds.
        assert_eq!(u.word(10.0), Some(1));
        // A mark wins over any guess, and can't run off the end.
        u.marked = Some(1);
        assert_eq!(u.word(10.0), Some(1));
        u.marked = Some(9);
        assert_eq!(u.word(10.0), Some(1));
        // Nothing is heard before the daemon says it has begun.
        u.started = None;
        assert_eq!(u.word(10.0), None);
    }

    #[test]
    fn a_pause_stops_the_clock() {
        let mut u = Utterance {
            id: 1,
            key: 0,
            first_word: 0,
            weights: vec![3, 3, 3],
            started: Some(Instant::now() - Duration::from_millis(40)),
            paused_since: None,
            paused_for: Duration::ZERO,
            marked: None,
        };
        u.set_paused(true);
        let at = u.word(10.0);
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(u.word(10.0), at, "a paused voice is still on its word");
        u.set_paused(false);
        assert!(u.paused_for >= Duration::from_millis(60));
    }

    #[test]
    fn reads_the_address_it_is_given() {
        assert_eq!(
            address_path("unix_socket:/tmp/speechd.sock").unwrap(),
            PathBuf::from("/tmp/speechd.sock")
        );
        assert!(address_path("inet_socket:localhost:6560").is_err());
        assert!(address_path("/tmp/speechd.sock").is_err());
    }
}

/// Needs a running speech-dispatcher and speaks out loud:
/// `cargo test -p vox_scribe -- --ignored spd_`.
#[cfg(test)]
mod daemon_tests {
    use super::*;

    #[test]
    #[ignore]
    fn spd_hears_words_in_order() {
        let voice = Spd::open(None).unwrap();
        eprintln!(
            "{} voices, starting in {}",
            voice.choices().len(),
            voice.name()
        );
        voice.say(7, "One two three.");
        voice.say(7, "Four five six seven.");
        let mut seen: Vec<usize> = Vec::new();
        let t = Instant::now();
        while voice.busy() && t.elapsed() < Duration::from_secs(60) {
            if let Some((key, i)) = voice.heard() {
                assert_eq!(key, 7);
                if seen.last() != Some(&i) {
                    seen.push(i);
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        eprintln!("heard {seen:?}");
        assert!(!voice.busy(), "the daemon finished speaking");
        assert!(seen.windows(2).all(|w| w[0] < w[1]), "words come in order");
        assert_eq!(seen.first(), Some(&0));
        assert_eq!(seen.last(), Some(&6));
        assert_eq!(voice.heard(), None);

        // Stopped and said again (space in --chat): counts from 0.
        voice.say(7, "Eight nine ten.");
        std::thread::sleep(Duration::from_millis(1500));
        voice.stop();
        voice.say(7, "Again from the top.");
        let mut first = None;
        while voice.busy() && first.is_none() {
            first = voice.heard();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(first, Some((7, 0)));
        voice.stop();
    }
}
