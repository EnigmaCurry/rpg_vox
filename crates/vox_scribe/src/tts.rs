//! Which engine reads replies aloud, and the voice `--chat` speaks
//! through.
//!
//! Two engines can speak:
//!
//! * **speech-dispatcher** ([`crate::spd`]) — the desktop's own speech
//!   daemon, in the voices it is already set up with. Nothing to
//!   download, and it is what a screen reader on the same machine uses.
//! * **Kokoro** ([`crate::voice::Kokoro`]) — the 82M model scribe
//!   fetches once (about 350 MB) and runs on the CPU. Works anywhere,
//!   sounds the same everywhere.
//!
//! `--tts auto`, the default, asks speech-dispatcher first and falls
//! back to Kokoro, so a machine where speech is already set up speaks
//! without downloading anything. It only asks: a daemon that isn't
//! running is left alone rather than started.
//!
//! Reading a transcript aloud (`--play NAME` with no NAME.opus) is
//! Kokoro's alone, whichever engine is picked: that player seeks and
//! stretches the samples, and speech-dispatcher hands none back — it
//! plays its speech itself.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{bail, Result};

/// Kokoro's voice when nothing else is asked for.
const KOKORO_DEFAULT: &str = "af_heart";

/// `--tts`: which engine speaks.
#[derive(Copy, Clone, PartialEq, Eq, Debug, clap::ValueEnum)]
pub enum Engine {
    /// speech-dispatcher if it answers, else Kokoro.
    Auto,
    /// The desktop's speech daemon, in its own voices.
    #[value(alias = "spd")]
    SpeechDispatcher,
    /// The Kokoro 82M model, downloaded once and run here.
    Kokoro,
}

/// One voice an engine offers, for `--voices` and the `v` menu.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    /// What `--voices` and the chat's database call it.
    pub name: String,
    /// How the `v` menu shows it.
    pub label: String,
}

/// The engine that will actually speak: `auto` asks the daemon, the
/// others are taken as given.
pub fn resolve(engine: Engine) -> Engine {
    match engine {
        Engine::Auto => {
            #[cfg(unix)]
            if crate::spd::detect() {
                return Engine::SpeechDispatcher;
            }
            Engine::Kokoro
        }
        chosen => chosen,
    }
}

/// A voice to read replies in, whichever engine makes it.
pub enum Voice {
    Kokoro(crate::voice::Kokoro),
    #[cfg(unix)]
    Spd(crate::spd::Spd),
}

impl Voice {
    /// Queue `text` to be spoken after anything already queued. Its
    /// words are counted on from earlier text with the same `key`.
    pub fn say(&self, key: i64, text: &str) {
        match self {
            Voice::Kokoro(v) => v.say(key, text),
            #[cfg(unix)]
            Voice::Spd(v) => v.say(key, text),
        }
    }

    /// The word being heard now: (key, its index under that key).
    pub fn heard(&self) -> Option<(i64, usize)> {
        match self {
            Voice::Kokoro(v) => v.heard(),
            #[cfg(unix)]
            Voice::Spd(v) => v.heard(),
        }
    }

    /// Something is queued, being made, or playing (paused or not).
    pub fn busy(&self) -> bool {
        match self {
            Voice::Kokoro(v) => v.busy(),
            #[cfg(unix)]
            Voice::Spd(v) => v.busy(),
        }
    }

    /// Whether this engine can be held mid-word and then go on from
    /// there — what `--chat` ducks a reply with while it works out
    /// whether a sound over it was really an interruption.
    ///
    /// Kokoro can: the samples are ours, so pausing is a gain ramp and
    /// the ring is left where it is. speech-dispatcher cannot be used
    /// that way, even though `PAUSE`/`RESUME` work on their own:
    /// cancelling a message that has been paused wedges the client,
    /// and pocket-tts then takes every message after it and says
    /// nothing — `BEGIN` and `END` in the same instant. Nothing
    /// recovers it short of a new connection. So a voice that cannot
    /// pause is simply left talking until the words are in, and then
    /// cut off for good.
    pub fn can_pause(&self) -> bool {
        match self {
            Voice::Kokoro(_) => true,
            #[cfg(unix)]
            Voice::Spd(_) => false,
        }
    }

    pub fn set_paused(&self, paused: bool) {
        match self {
            Voice::Kokoro(v) => v.set_paused(paused),
            #[cfg(unix)]
            Voice::Spd(v) => v.set_paused(paused),
        }
    }

    /// Speak in `name` from the next sentence on; false if this engine
    /// has no such voice (a voice saved by the other one, say).
    pub fn set_voice(&self, name: &str) -> bool {
        match self {
            Voice::Kokoro(v) => v.set_voice(name),
            #[cfg(unix)]
            Voice::Spd(v) => v.set_voice(name),
        }
    }

    /// Drop everything not yet heard.
    pub fn stop(&self) {
        match self {
            Voice::Kokoro(v) => v.stop(),
            #[cfg(unix)]
            Voice::Spd(v) => v.stop(),
        }
    }

    /// Which engine is speaking, for the `v` menu to say.
    pub fn engine(&self) -> &'static str {
        match self {
            Voice::Kokoro(_) => "kokoro",
            #[cfg(unix)]
            Voice::Spd(_) => "speech-dispatcher",
        }
    }

    /// The voices `v` offers.
    pub fn choices(&self) -> &[Choice] {
        match self {
            Voice::Kokoro(v) => v.choices(),
            #[cfg(unix)]
            Voice::Spd(v) => v.choices(),
        }
    }
}

/// The voice `--chat` replies in: the first of `--voices`, else the one
/// chosen for this conversation with `v`, else the engine's own default
/// (af_heart for Kokoro, whatever the daemon is set to otherwise). Also
/// returns its name, for the `v` menu to mark.
pub fn chat_voice(
    engine: Engine,
    models_dir: &Path,
    voices: &[String],
    db: &Path,
) -> Result<(Voice, String)> {
    let asked = voices.first().map(String::as_str);
    // The saved voice is only looked up when --voices doesn't say.
    let saved = match asked {
        Some(_) => None,
        None => {
            let db = crate::chatdb::Db::open(db)?;
            crate::chat::saved_voice(&db, db.current()?)?
        }
    };
    let wanted = asked.or(saved.as_deref());
    match resolve(engine) {
        #[cfg(unix)]
        Engine::SpeechDispatcher => {
            let voice = match crate::spd::Spd::open(wanted) {
                Ok(voice) => voice,
                // A voice saved while the other engine spoke (or from a
                // module since removed) is no reason not to open the
                // chat; one named on the command line is.
                Err(e) if asked.is_none() => {
                    eprintln!("{e:#}; speaking in speech-dispatcher's own voice");
                    crate::spd::Spd::open(None)?
                }
                Err(e) => return Err(e),
            };
            let name = voice.name().to_string();
            Ok((Voice::Spd(voice), name))
        }
        #[cfg(not(unix))]
        Engine::SpeechDispatcher => {
            bail!("speech-dispatcher speaks on unix only; try --tts kokoro")
        }
        _ => {
            let name = wanted.unwrap_or(KOKORO_DEFAULT);
            let (name, sid) = match (vox_transcribe::tts::voice_id(name), asked) {
                (Some(sid), _) => (name, sid),
                (None, Some(_)) => bail!("no Kokoro voice {name:?}; `scribe voices` lists them"),
                (None, None) => {
                    eprintln!("no Kokoro voice {name:?}; speaking in {KOKORO_DEFAULT}");
                    let sid = vox_transcribe::tts::voice_id(KOKORO_DEFAULT)
                        .expect("af_heart is one of Kokoro's voices");
                    (KOKORO_DEFAULT, sid)
                }
            };
            let threads = std::thread::available_parallelism()
                .map(|n| n.get().min(4))
                .unwrap_or(2) as i32;
            let tts =
                vox_transcribe::tts::Kokoro::load(&crate::models::kokoro_dir(models_dir), threads)?;
            Ok((
                Voice::Kokoro(crate::voice::Kokoro::new(tts, sid)?),
                name.to_string(),
            ))
        }
    }
}

/// `scribe voices`: the names `--voices` takes, for the engine that
/// would speak. Kokoro's are built in; speech-dispatcher's are asked of
/// the daemon, which has to be running.
pub fn list(engine: Engine) -> Result<()> {
    match resolve(engine) {
        #[cfg(unix)]
        Engine::SpeechDispatcher => {
            let (module, choices) = crate::spd::voices()?;
            println!("speech-dispatcher, through its {module} module:");
            for choice in choices {
                println!("  {:<48} {}", choice.name, choice.label);
            }
        }
        #[cfg(not(unix))]
        Engine::SpeechDispatcher => {
            bail!("speech-dispatcher speaks on unix only; try --tts kokoro")
        }
        _ => {
            println!("Kokoro:");
            for choice in crate::voice::choices() {
                println!("  {:<48} {}", choice.name, choice.label);
            }
        }
    }
    Ok(())
}

/// One less, never below zero. A plain compare-exchange loop:
/// `fetch_update` is deprecated on new toolchains, and its replacement
/// `try_update` is missing on older ones.
pub fn one_less(n: &AtomicUsize) {
    let mut have = n.load(Ordering::SeqCst);
    while have > 0 {
        match n.compare_exchange_weak(have, have - 1, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => break,
            Err(now) => have = now,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_down_to_zero_and_stays_there() {
        let n = AtomicUsize::new(1);
        one_less(&n);
        assert_eq!(n.load(Ordering::SeqCst), 0);
        one_less(&n);
        assert_eq!(n.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn an_engine_asked_for_is_the_engine_used() {
        assert_eq!(resolve(Engine::Kokoro), Engine::Kokoro);
        assert_eq!(resolve(Engine::SpeechDispatcher), Engine::SpeechDispatcher);
        // auto depends on the machine: either answer, never auto.
        assert_ne!(resolve(Engine::Auto), Engine::Auto);
    }
}

/// Needs a running speech-dispatcher (but says nothing aloud):
/// `cargo test -p vox_scribe -- --ignored chat_voice`.
#[cfg(test)]
mod daemon_tests {
    use super::*;

    /// A chat database of its own, taken away with the test.
    struct TempDb(std::path::PathBuf);

    impl TempDb {
        fn new(name: &str) -> Self {
            Self(std::env::temp_dir().join(format!("scribe-tts-{}-{name}.db", std::process::id())))
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let mut path = self.0.clone().into_os_string();
                path.push(suffix);
                let _ = std::fs::remove_file(path);
            }
        }
    }

    #[test]
    #[ignore]
    fn chat_voice_speaks_in_what_the_daemon_has() {
        let db = TempDb::new("fresh");
        // No models directory is needed: nothing is loaded here.
        let (voice, name) =
            chat_voice(Engine::SpeechDispatcher, Path::new("/nowhere"), &[], &db.0).unwrap();
        assert!(!voice.choices().is_empty(), "the daemon offers voices");
        assert!(!name.is_empty(), "the voice in use has a name to show");
        // Kokoro's names mean nothing here; the daemon's own do.
        assert!(!voice.set_voice("af_heart"));
        let offered = voice.choices()[0].name.clone();
        assert!(voice.set_voice(&offered));
    }

    #[test]
    #[ignore]
    fn chat_voice_keeps_a_voice_it_cannot_speak_from_closing_the_chat() {
        let db = TempDb::new("stale");
        {
            // As a chat spoken by Kokoro would have left it.
            let open = crate::chatdb::Db::open(&db.0).unwrap();
            let conv = open.current().unwrap();
            open.set_setting(&format!("voice.{conv}"), "af_heart")
                .unwrap();
        }
        let (voice, _) =
            chat_voice(Engine::SpeechDispatcher, Path::new("/nowhere"), &[], &db.0).unwrap();
        assert!(!voice.choices().is_empty());

        // Named on the command line, though, it is an error worth saying.
        let asked = [String::from("af_heart")];
        let err = match chat_voice(
            Engine::SpeechDispatcher,
            Path::new("/nowhere"),
            &asked,
            &db.0,
        ) {
            Ok(_) => panic!("a voice the daemon hasn't got is an error"),
            Err(e) => e,
        };
        assert!(format!("{err:#}").contains("af_heart"), "{err:#}");
    }
}
