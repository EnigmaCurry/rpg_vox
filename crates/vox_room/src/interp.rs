//! The interpreters: one voice per language the room is listening in,
//! saying what people say to everybody who doesn't speak it.
//!
//! One thread per language, because a language is one voice and it
//! speaks in turn -- what was said has to be heard in the order it was
//! said. They are separate from the AI's voice threads so neither waits
//! on the other; the mixer is where the two meet, and it holds an
//! interpretation back while the AI is talking in the same language.

use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};
use tracing::{info, warn};
use vox_transcribe::translate::Translate;

use crate::room::{InterpJob, Room};
use crate::tts::Voice;

/// Utterances waiting to be interpreted into one language. Bounded, and
/// short: an interpreter this far behind is one nobody can follow, so
/// the room drops rather than let the queue grow without end. The words
/// are on screen either way.
const QUEUE: usize = 16;

/// Lines kept as context for the translator. Enough for a pronoun to
/// find what it refers to, not so many that an old turn colours a new
/// one.
const CONTEXT: usize = 4;

pub fn queue() -> (Sender<InterpJob>, Receiver<InterpJob>) {
    crossbeam_channel::bounded(QUEUE)
}

/// Start the interpreter for one language. `voice` is already set to
/// speak it, so the language is never passed per sentence.
pub fn spawn(
    room: Arc<Room>,
    language: &str,
    mut voice: Box<dyn Voice>,
    translate: Arc<dyn Translate>,
    rx: Receiver<InterpJob>,
) {
    let language = language.to_string();
    std::thread::Builder::new()
        .name(format!("vox-room-interp-{language}"))
        .spawn(move || {
            let mut context: Vec<String> = Vec::new();
            for job in rx {
                // Everybody who wanted it has gone, or changed what they
                // are listening in. Checked here rather than only at the
                // door, because a queue takes time to work through.
                if !room.is_heard_in(&job.into) {
                    room.interp_failed(&job.entry, &job.into);
                    continue;
                }
                let started = Instant::now();
                let say = match translate.translate(&job.text, &job.from, &job.into, &context) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(
                            who = %job.who,
                            language = %job.into,
                            "couldn't interpret {:?}: {e:#}", job.text
                        );
                        room.interp_failed(&job.entry, &job.into);
                        continue;
                    }
                };
                let translated_ms = started.elapsed().as_millis() as u64;
                context.push(say.clone());
                if context.len() > CONTEXT {
                    context.remove(0);
                }
                match voice.speak(&say, None) {
                    Ok(samples) => {
                        info!(
                            who = %job.who,
                            from = %job.from,
                            into = %job.into,
                            chars = say.len(),
                            translated_ms,
                            ms = started.elapsed().as_millis() as u64,
                            audio_ms = samples.len() as u64 * 1000 / crate::mixer::RATE as u64,
                            "interpreted"
                        );
                        room.interp_spoke(&job.entry, &job.into, &say, samples);
                    }
                    Err(e) => {
                        warn!(language = %job.into, "the interpreter's voice failed on {say:?}: {e:#}");
                        room.interp_failed(&job.entry, &job.into);
                    }
                }
            }
        })
        .expect("spawn interpreter thread");
}
