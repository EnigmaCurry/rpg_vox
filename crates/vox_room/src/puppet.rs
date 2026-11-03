//! Puppets: characters the first AI speaks through, and the shows it
//! puts them in.
//!
//! A bot has a brain of its own. Two of them in a room are two models
//! talking to each other, a request each per line, each deciding for
//! itself what to say next -- which is the point of them, and which is
//! also why they cannot act out a scene. Neither one knows what the
//! other is about to say, so there is no timing to it: every line waits
//! for a model to finish thinking, and an interjection lands wherever
//! the round trip put it.
//!
//! A puppet is the other half of that. It has a name, a face, a voice
//! and a fader, exactly like a bot -- to anybody in the room it *is*
//! another voice in the room -- and nothing behind it at all. What it
//! says is the first AI's, written in advance and said through it. So a
//! room can have a play in it: the AI writes the whole script, every
//! line is turned into audio *before* the one in front of it is
//! finished, and the lines land where the script says they land,
//! including on top of each other.
//!
//! The thread here is the puppeteer. There is one per room that has
//! ever wanted one, and it does two things:
//!
//! * **A show.** A whole script at once. It renders a line or two
//!   ahead, then plays the lines in order: each one begins when the one
//!   before it has been heard out (plus whatever pause the script
//!   asks), or cuts in over the top of it at a given moment, which is
//!   what an interruption is. The waiting *is* the rendering budget --
//!   the audio for line four is made while line three is being heard --
//!   so a play runs at the speed of speech rather than the speed of the
//!   speech server.
//!
//! * **One line, now.** What the AI does after the show, when the
//!   puppets are still on stage taking questions: it answers as one of
//!   them instead of in its own voice. Same path, one line at a time,
//!   no script.
//!
//! Everything a line *is* on screen -- the entry with the puppet's name
//! on it, the words appearing as the audio plays, the dash where it was
//! cut off, the line in the history afterwards -- is the room's own
//! reply machinery, used exactly as a bot's reply uses it. See
//! [`Room::puppet_line`]. The only thing that is different about a
//! puppet is that nothing ever gives it the turn.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TrySendError};
use serde_json::Value;
use tracing::{debug, info, warn};
use vox_transcribe::translate::Translate;

use crate::room::{Room, Speaking};
use crate::tts::Voice;

/// Most lines one script may have. Long enough for a scene with a
/// beginning and an end, short enough that the AI has to decide what
/// the play is about rather than let it run.
pub const SCRIPT_MAX: usize = 120;

/// Longest one line may be. A line is one character speaking without
/// being interrupted; anything longer is a monologue, and a monologue
/// is written as several lines so the others can cut into it.
pub const LINE_MAX: usize = 600;

/// Longest pause a script may ask for between two lines. A beat, not
/// an interval.
const PAUSE_MAX: Duration = Duration::from_secs(10);

/// Longest a line may come in over the one before it. An interjection
/// lands on the last word or two; anything longer is two people
/// talking at once for so long that neither can be heard, which is a
/// thing a play does on purpose about once.
const OVER_MAX: Duration = Duration::from_secs(3);

/// The pause between two lines when the script doesn't say: long
/// enough to hear as two people rather than one, short enough not to
/// read as hesitation.
const PAUSE_DEFAULT: Duration = Duration::from_millis(350);

/// Lines rendered before the curtain goes up. One would do -- the
/// second is made while the first is being heard -- but a short
/// opening line leaves no time to make a long second one, and a gap
/// after the first line of a play is the one gap everybody notices.
const PRELOAD: usize = 2;

/// Rendered lines held at once. The ceiling on how far ahead the
/// puppeteer may work: the audio is a few hundred kilobytes a line,
/// and nothing is gained by making the whole play before saying any of
/// it.
const LOOKAHEAD: usize = 6;

/// How often the puppeteer looks to see whether it is time for the
/// next line, when it has nothing left to render. The room reports
/// playback every 50 ms, so there is nothing finer to wait for.
const TICK: Duration = Duration::from_millis(20);

/// How long the puppeteer waits for the AI to finish announcing the
/// show before it starts anyway. A play that began over the top of
/// "right, here we go" would be two voices at once; a play that never
/// began because a voice thread died would be worse.
const CURTAIN_WAIT: Duration = Duration::from_secs(90);

/// Jobs waiting for the puppeteer. Bounded, and small: these are
/// things somebody asked for out loud, and a queue of them deeper than
/// this is a queue nobody is still waiting on.
const QUEUE: usize = 16;

/// One line of a script: who says it, what they say, and when.
#[derive(Debug, Clone)]
pub struct Line {
    /// The voice that says it: a puppet on stage, or the AI running
    /// the show in its own voice.
    pub who: String,
    pub say: String,
    /// Silence before this line, measured from the moment the line
    /// before it was heard out.
    pub pause: Duration,
    /// Come in over the tail of the line before this one, this far
    /// before its end, instead of waiting for it. Two voices at once
    /// for that long, which is what an interjection sounds like.
    ///
    /// Not a cut: nothing is thrown away and nothing is cut off. An
    /// interruption is written into the script as a line that stops
    /// where the character was interrupted -- "But I only wanted--" --
    /// and delivered whole. The room used to do it the other way, by
    /// synthesizing the whole line and then cutting the audio off with
    /// the same machinery a person talking over a bot uses, and that
    /// was wrong twice over: the cut landed where the wall clock said
    /// rather than where the writer meant, and most of a line the
    /// speech server had been paid for was thrown away. A playwright
    /// knows where the sentence breaks; nothing else does.
    ///
    /// None is the ordinary case: wait, then speak.
    pub over: Option<Duration>,
}

/// A play: what it is called and what is said in it.
#[derive(Debug, Clone)]
pub struct Script {
    pub title: String,
    pub lines: Vec<Line>,
}

impl Script {
    /// The voices in it, in the order they first speak.
    pub fn cast(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for l in &self.lines {
            if !out.contains(&l.who) {
                out.push(l.who.clone());
            }
        }
        out
    }

    /// Read a script out of what the model sent, and say plainly what
    /// is wrong with it rather than play half of it.
    ///
    /// `may_say` is who is allowed a line: the AI running the show and
    /// the puppets on stage. Not another bot -- that one has a brain of
    /// its own, and a script that put words in its mouth would be this
    /// AI answering for it.
    pub fn parse(title: &str, lines: &Value, may_say: &[String]) -> Result<Self, String> {
        let title = title.trim();
        if title.is_empty() {
            return Err("the show needs a name: it goes on the card in the chat".into());
        }
        let Some(said) = lines.as_array() else {
            return Err("`script` is a list of lines, each with `who` and `say`".into());
        };
        if said.is_empty() {
            return Err("the script has no lines in it".into());
        }
        if said.len() > SCRIPT_MAX {
            return Err(format!(
                "a script is at most {SCRIPT_MAX} lines; this one has {}. Put the \
                 rest on as a second scene once this one has played",
                said.len()
            ));
        }
        let mut out = Vec::new();
        for (n, line) in said.iter().enumerate() {
            let at = n + 1;
            let who = line["who"].as_str().unwrap_or("").trim();
            let say = line["say"].as_str().unwrap_or("").trim();
            if who.is_empty() || say.is_empty() {
                return Err(format!("line {at} needs both `who` and `say`"));
            }
            let Some(who) = may_say.iter().find(|w| w.eq_ignore_ascii_case(who)) else {
                return Err(format!(
                    "line {at} is {who:?}, who is not in the show. The lines can be \
                     yours or any puppet on stage: {}",
                    crate::room::and_list(may_say)
                ));
            };
            if say.chars().count() > LINE_MAX {
                return Err(format!(
                    "line {at} is {} characters, and a line is at most {LINE_MAX}. \
                     Break a long speech into several lines of its own -- that is \
                     also what lets somebody cut into it",
                    say.chars().count()
                ));
            }
            // Seconds, as the model thinks of them: "a beat" is 0.3,
            // "he waits" is 2.
            let pause = match line["pause"].as_f64() {
                Some(s) if s.is_finite() && s >= 0.0 => Duration::from_secs_f64(s).min(PAUSE_MAX),
                Some(_) => return Err(format!("line {at}: `pause` is seconds, from 0")),
                None => PAUSE_DEFAULT,
            };
            let over = match line["over"].as_f64() {
                Some(s) if s.is_finite() && s >= 0.0 => {
                    Some(Duration::from_secs_f64(s).min(OVER_MAX))
                }
                Some(_) => {
                    return Err(format!(
                        "line {at}: `over` is how many seconds before the end of the \
                         line before this one to come in, from 0"
                    ))
                }
                None => None,
            };
            // Nothing to come in over: the first line of the play, or
            // a character coming in over itself, which is one voice
            // and one strip and cannot be two at once. Dropped rather
            // than refused.
            //
            // It was a refusal, and a model that fills in every field
            // the schema offers -- which is what a model does -- could
            // not work out from the words "nothing to cut into" that
            // the fix was to leave the field off. It sent the same
            // script four times.
            let same = n > 0 && out.last().is_some_and(|l: &Line| &l.who == who);
            let over = match (n, same, over) {
                (0, _, Some(_)) | (_, true, Some(_)) => {
                    debug!(line = at, "nothing for this line to come in over");
                    None
                }
                (_, _, o) => o,
            };
            out.push(Line {
                who: who.clone(),
                say: say.to_string(),
                pause,
                over,
            });
        }
        Ok(Self {
            title: title.to_string(),
            lines: out,
        })
    }
}

/// What the puppeteer has been asked to do.
enum Job {
    /// Put a whole script on.
    Show(Script),
    /// Say one line as one voice, now. For after the show, when the
    /// puppets are still there and somebody has asked one of them
    /// something.
    Say { who: String, say: String },
}

/// The puppeteer's end of the room. Made the first time an AI wants a
/// puppet, and kept by that AI's agent thread: one per room, because a
/// stage with two hands on it is two plays at once.
#[derive(Clone)]
pub struct Puppeteer {
    jobs: Sender<Job>,
}

impl Puppeteer {
    /// Start the thread. `translate` is how a line reaches whoever is
    /// listening in another language, or None in a house with no way
    /// to translate -- and then a puppet is heard in the room's own
    /// language and read in everybody else's.
    pub fn spawn(room: Arc<Room>, translate: Option<Arc<dyn Translate>>) -> Self {
        let (jobs, rx) = crossbeam_channel::bounded(QUEUE);
        let name = room.config().id.clone();
        std::thread::Builder::new()
            .name(format!("vox-room-puppets-{name}"))
            .spawn(move || work(room, translate, rx))
            .expect("spawn puppeteer thread");
        Self { jobs }
    }

    /// Put a show on. The answer comes back at once: the play is the
    /// thread's from here, and the AI is given the turn again when the
    /// curtain comes down (see [`Room::end_show`]).
    pub fn show(&self, script: Script) -> Result<(), String> {
        self.send(Job::Show(script))
    }

    /// Say one line as one of the puppets, now.
    pub fn say(&self, who: &str, say: &str) -> Result<(), String> {
        self.send(Job::Say {
            who: who.to_string(),
            say: say.to_string(),
        })
    }

    fn send(&self, job: Job) -> Result<(), String> {
        match self.jobs.try_send(job) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err("the puppets have more to say than they \
                                               can get through; wait for them"
                .to_string()),
            Err(TrySendError::Disconnected(_)) => {
                Err("the puppeteer has stopped: restart the room".to_string())
            }
        }
    }
}

fn work(room: Arc<Room>, translate: Option<Arc<dyn Translate>>, jobs: Receiver<Job>) {
    for job in jobs {
        match job {
            Job::Show(script) => {
                play(&room, &translate, &script);
                // However it went -- played out, stopped, or a voice
                // that failed on line one -- the curtain comes down
                // here and nowhere else. It is what puts the room's
                // settings back and gives the AI the turn to say what
                // happened, and a show that ended without it would
                // leave an AI answering nothing for good.
                room.end_show();
            }
            Job::Say { who, say } => {
                if let Err(err) = aside(&room, &translate, &who, &say) {
                    warn!(puppet = %who, "couldn't say a line: {err}");
                    room.error(&format!("{who} couldn't say that: {err}"));
                }
            }
        }
    }
}

/// One sentence of one line, ready to be heard: which language it is
/// in, its place among that language's sentences, what it says, and
/// the audio.
struct Said {
    language: String,
    index: usize,
    text: String,
    samples: Vec<f32>,
}

/// Put a show on: render ahead, and play the lines where the script
/// says they go.
fn play(room: &Arc<Room>, translate: &Option<Arc<dyn Translate>>, script: &Script) {
    let canonical = room.config().language.clone();
    // One mouth per voice in the play, each with a voice per language
    // the room is spoken in. Built up front: a play that found out on
    // line nine that one of its characters had no voice would have
    // already been half performed.
    let mut mouths: BTreeMap<String, Speaking> = BTreeMap::new();
    for who in script.cast() {
        match room.stage_voices(&who) {
            Ok(v) => {
                mouths.insert(who, v);
            }
            Err(err) => {
                warn!(title = %script.title, %who, "the show can't go on: {err}");
                room.error(&format!(
                    "{who} has no voice, so the show can't go on: {err}"
                ));
                return;
            }
        }
    }
    // Lines already turned into audio, by their place in the script.
    let mut ready: BTreeMap<usize, Vec<Said>> = BTreeMap::new();
    let mut made = 0usize;
    while made < script.lines.len() && made < PRELOAD {
        render(
            room,
            translate,
            &canonical,
            &mut mouths,
            script,
            made,
            &mut ready,
        );
        made += 1;
    }
    // The AI is probably still saying what the play is. Its own voice
    // and the first line of the play over the top of each other is the
    // one thing this cannot do.
    let director = room.config().bot().name.clone();
    let waited = Instant::now();
    while room.voice_busy(&director) && waited.elapsed() < CURTAIN_WAIT {
        if !render_ahead(
            room,
            translate,
            &canonical,
            &mut mouths,
            script,
            &mut made,
            &mut ready,
        ) {
            std::thread::sleep(TICK);
        }
        if room.show_stopped() {
            return;
        }
    }
    // How long the line being heard is, and when it was pushed. The
    // strip was empty when it went on, so the two together say when it
    // ends -- which is what a line coming in over its tail is timed
    // against, and it is exact: the length is a count of samples, not
    // a guess about the clock.
    let mut pushed = Instant::now();
    let mut playing = Duration::ZERO;
    let mut before: Option<String> = None;
    for (n, line) in script.lines.iter().enumerate() {
        // Spend every wait rendering the lines after this one: the
        // waiting in a play *is* the rendering budget, which is what
        // lets the audio for line four be made while line three is
        // being heard.
        let spin = |room: &Room,
                    mouths: &mut BTreeMap<String, Speaking>,
                    made: &mut usize,
                    ready: &mut BTreeMap<usize, Vec<Said>>,
                    done: &mut dyn FnMut(&Room) -> bool|
         -> bool {
            while !done(room) {
                if room.show_stopped() {
                    return false;
                }
                if !render_ahead(room, translate, &canonical, mouths, script, made, ready) {
                    std::thread::sleep(TICK);
                }
            }
            !room.show_stopped()
        };

        // First: the line in front of this one is out of the way.
        //
        // Either it has been heard to the end, or -- for a line coming
        // in over it -- it has `over` left to run. Nothing is ever cut
        // off here: an interruption is a line the script wrote short.
        let clear = match (&before, line.over) {
            (Some(_), Some(over)) => {
                let start = playing.saturating_sub(over);
                let mut when = |_: &Room| pushed.elapsed() >= start;
                spin(room, &mut mouths, &mut made, &mut ready, &mut when)
            }
            (Some(was), None) => {
                let was = was.clone();
                let mut when = |room: &Room| !room.voice_busy(&was);
                spin(room, &mut mouths, &mut made, &mut ready, &mut when)
            }
            // The first line of the play: there is nothing in front of
            // it, so its pause is the beat before the curtain.
            (None, _) => true,
        };
        if !clear {
            info!(title = %script.title, said = n, "the show was stopped");
            return;
        }

        // Then the pause, measured from now -- which is the moment the
        // line before it was heard out, and not the moment it started.
        //
        // That is the whole of what `pause` is for, and it used to be
        // measured from the push instead: by the time a line of any
        // length had drained, the pause had already elapsed and every
        // line ran back to back whatever the script asked for.
        if !line.pause.is_zero() && line.over.is_none() {
            let until = Instant::now() + line.pause;
            let mut when = |_: &Room| Instant::now() >= until;
            if !spin(room, &mut mouths, &mut made, &mut ready, &mut when) {
                info!(title = %script.title, said = n, "the show was stopped");
                return;
            }
        }

        // Its own audio, which the waiting above should have made long
        // since. Rendered here if it hasn't -- a line longer to
        // synthesize than everything in front of it was to say, which
        // is a gap in the play and not a failure of it.
        while !ready.contains_key(&n) && made <= n {
            render(
                room,
                translate,
                &canonical,
                &mut mouths,
                script,
                made,
                &mut ready,
            );
            made += 1;
        }
        let said = ready.remove(&n).unwrap_or_default();
        if said.is_empty() {
            warn!(title = %script.title, line = n + 1, "nothing to say: skipped");
            continue;
        }
        let Some(long) = utter(room, &canonical, &line.who, said) else {
            // It left the stage mid-play. The rest of its lines go
            // with it, and whatever the others had to say still gets
            // said: a play missing a character is better than a play
            // that stopped.
            warn!(
                title = %script.title,
                who = %line.who,
                line = n + 1,
                "that one isn't in the room any more"
            );
            continue;
        };
        pushed = Instant::now();
        playing = long;
        before = Some(line.who.clone());
        room.show_said(n + 1);
    }
    // The last line has been queued, not heard. The curtain waits for
    // it: an AI given the turn to ask what everyone thought, over the
    // top of the closing line, is not the end of a play.
    if let Some(last) = &before {
        let out = Instant::now();
        while room.voice_busy(last) && out.elapsed() < CURTAIN_WAIT {
            if room.show_stopped() {
                return;
            }
            std::thread::sleep(TICK);
        }
    }
    info!(title = %script.title, lines = script.lines.len(), "the show played out");
}

/// Render the next unrendered line, if there is one and there is room
/// for it. True when something was made, which is what says whether
/// the caller has to sleep: the waiting in a play is the rendering
/// budget, and a sleep in the middle of it is a line not being made.
fn render_ahead(
    room: &Room,
    translate: &Option<Arc<dyn Translate>>,
    canonical: &str,
    mouths: &mut BTreeMap<String, Speaking>,
    script: &Script,
    made: &mut usize,
    ready: &mut BTreeMap<usize, Vec<Said>>,
) -> bool {
    if *made >= script.lines.len() || ready.len() >= LOOKAHEAD {
        return false;
    }
    render(room, translate, canonical, mouths, script, *made, ready);
    *made += 1;
    true
}

/// Turn one line into audio, in every language the room is heard in.
fn render(
    room: &Room,
    translate: &Option<Arc<dyn Translate>>,
    canonical: &str,
    mouths: &mut BTreeMap<String, Speaking>,
    script: &Script,
    n: usize,
    ready: &mut BTreeMap<usize, Vec<Said>>,
) {
    let Some(line) = script.lines.get(n) else {
        return;
    };
    let Some(mouth) = mouths.get_mut(&line.who) else {
        return;
    };
    let (said, failed) = voiced(room, translate, canonical, mouth, &line.who, &line.say);
    if !said.is_empty() {
        ready.insert(n, said);
        return;
    }
    // Nothing at all, so this line is not going to be heard. Said in
    // the room once, with the reason: a line missing from a play is
    // the sort of thing everybody watching notices and nobody can
    // explain, and the reason is usually something the AI can put
    // right between shows.
    if let Some(why) = failed {
        room.error(&format!("{} couldn't say a line: {why}", line.who));
    }
}

/// One line, spoken into samples: each sentence of it, in each
/// language somebody in the room is listening in.
///
/// Deliberately the same shape as the voice thread an AI's reply goes
/// through -- translate, then speak, with what has already been said
/// as context -- because it is the same job. What is different is that
/// nothing is handed to the room here: the audio comes back, to be
/// played when the script says, which is the whole point of a play.
///
/// The second half of the answer is the first thing that went wrong,
/// if anything did. It is carried out rather than only logged because
/// the one who can do something about it is the model: a puppet built
/// on a voice the server has never heard of fails with the server's
/// own list of voices in the refusal, and an AI shown that will fix
/// the puppet. An AI told "its voice said nothing at all" will try
/// again and fail again.
fn voiced(
    room: &Room,
    translate: &Option<Arc<dyn Translate>>,
    canonical: &str,
    mouth: &mut [(String, Box<dyn Voice>)],
    who: &str,
    say: &str,
) -> (Vec<Said>, Option<String>) {
    let mut out = Vec::new();
    let mut failed: Option<String> = None;
    for (language, voice) in mouth.iter_mut() {
        let own = language == canonical;
        // Nobody here wants it in this language: no translation and no
        // voice, rather than paying for both and playing to an empty
        // queue. The room's own language is always made -- it is what
        // the line *is*, and what the log follows.
        if !own && !room.is_heard_in(language) {
            continue;
        }
        let mut context: Vec<String> = Vec::new();
        for (index, sentence) in vox_transcribe::filters::sentences(say)
            .into_iter()
            .enumerate()
        {
            let text = match (own, translate) {
                (true, _) | (false, None) => sentence.to_string(),
                (false, Some(t)) => match t.translate(sentence, canonical, language, &context) {
                    Ok(s) => s,
                    Err(err) => {
                        warn!(%who, %language, "couldn't translate {sentence:?}: {err:#}");
                        failed.get_or_insert_with(|| format!("{err:#}"));
                        continue;
                    }
                },
            };
            context.push(text.clone());
            let started = Instant::now();
            match voice.speak(&text, (!own).then_some(language.as_str())) {
                Ok(samples) => {
                    info!(
                        %who,
                        %language,
                        chars = text.len(),
                        ms = started.elapsed().as_millis() as u64,
                        audio_ms = samples.len() as u64 * 1000 / crate::mixer::RATE as u64,
                        "puppet: a line rendered"
                    );
                    out.push(Said {
                        language: language.clone(),
                        index,
                        text,
                        samples,
                    });
                }
                // One sentence of one line. The rest of the line is
                // still said: a play with a sentence missing is a play,
                // and a character who went silent halfway is not.
                Err(err) => {
                    warn!(%who, %language, "the voice failed on {text:?}: {err:#}");
                    failed.get_or_insert_with(|| format!("{err:#}"));
                }
            }
        }
    }
    (out, failed)
}

/// Hand a rendered line to the room, as a reply with that voice's name
/// on it. The answer is how long it will take to hear, which is what
/// the line after it is timed against; None when that voice is no
/// longer in the room.
fn utter(room: &Room, canonical: &str, who: &str, said: Vec<Said>) -> Option<Duration> {
    // Nothing in the room's own language is nothing said: that is the
    // language the line *is* and the one the log follows. Checked
    // before a turn is opened, so a voice that failed leaves no empty
    // bubble behind for the room to wonder about.
    if !said.iter().any(|s| s.language == canonical) {
        warn!(%who, "its voice made nothing in the room's own language");
        return None;
    }
    let g = room.puppet_line(who)?;
    // How long the room's own language takes to say, counted in the
    // samples actually queued. The strip was empty, so this is when
    // the line ends.
    let mut samples = 0usize;
    for s in said {
        if s.language == canonical {
            samples += s.samples.len();
            room.agent_speak(g, &s.text, s.samples);
        } else {
            room.agent_speak_translated(g, &s.language, s.index, &s.text, s.samples);
        }
    }
    // Everything there was is queued. The room's own language closes
    // the turn; every other language the room is spoken in says so for
    // itself, whether or not anything was made in it -- a turn left
    // waiting on a language nobody was listening in would never end.
    room.agent_done(g);
    for language in &room.config().spoken {
        if language != canonical {
            room.agent_done_in(g, language);
        }
    }
    Some(Duration::from_secs_f64(
        samples as f64 / crate::mixer::RATE as f64,
    ))
}

/// One line as one voice, now: what the AI does when a puppet left on
/// stage is asked something.
///
/// Rendered first and queued second. The other way round would mean
/// the voice before it fell silent while this one was being made, and
/// two puppets answering in turn would have a hole between every
/// answer.
fn aside(
    room: &Arc<Room>,
    translate: &Option<Arc<dyn Translate>>,
    who: &str,
    say: &str,
) -> Result<(), String> {
    let canonical = room.config().language.clone();
    let mut mouth = room.stage_voices(who)?;
    let (said, failed) = voiced(room, translate, &canonical, &mut mouth, who, say);
    if said.is_empty() {
        return Err(failed.unwrap_or_else(|| "its voice said nothing at all".to_string()));
    }
    // Wait for whoever is talking: another puppet it is answering
    // after, itself, or the AI holding it -- which is told to say
    // nothing in its own voice in a turn where it answers as a
    // character, and may do anyway. Not for a person: a puppet is a
    // voice in the room, and the room's own barge-in is what deals
    // with somebody talking over one.
    let out = Instant::now();
    let mut busy: BTreeSet<String> = room.on_stage().into_iter().collect();
    busy.insert(room.config().bot().name.clone());
    while out.elapsed() < CURTAIN_WAIT && busy.iter().any(|b| room.voice_busy(b)) {
        std::thread::sleep(TICK);
    }
    if utter(room, &canonical, who, said).is_none() {
        return Err(format!("{who} has left the stage"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::room::{BotConfig, Mode, RoomConfig};
    use crate::store::Store;
    use serde_json::json;

    fn lines(script: &[(&str, &str, Value)]) -> Value {
        Value::Array(
            script
                .iter()
                .map(|(who, say, extra)| {
                    let mut l = json!({ "who": who, "say": say });
                    if let Some(o) = extra.as_object() {
                        for (k, v) in o {
                            l[k] = v.clone();
                        }
                    }
                    l
                })
                .collect(),
        )
    }

    /// A script is read whole or refused whole, and the refusal says
    /// what to do about it: half a play is worse than none.
    #[test]
    fn a_script_is_read_whole_or_refused_with_a_reason() {
        let cast = ["Vox".to_string(), "Grimble".to_string()];
        let ok = Script::parse(
            "The Gate",
            &lines(&[
                ("Grimble", "None shall pass.", json!({})),
                ("Vox", "I only wanted--", json!({ "pause": 0 })),
                ("Grimble", "Then want it elsewhere.", json!({ "over": 0.4 })),
            ]),
            &cast,
        )
        .expect("a script three lines long");
        assert_eq!(ok.title, "The Gate");
        assert_eq!(ok.cast(), ["Grimble", "Vox"], "in the order they speak");
        assert_eq!(ok.lines[0].pause, PAUSE_DEFAULT, "a beat, unasked");
        assert_eq!(
            ok.lines[1].pause,
            Duration::ZERO,
            "and 0 means on the last word"
        );
        assert_eq!(ok.lines[2].over, Some(Duration::from_millis(400)));

        let why = |script: Value| Script::parse("The Gate", &script, &cast).unwrap_err();

        // Somebody who is not in the show. Another AI in the room has
        // a brain of its own and will answer for itself; a script that
        // put words in its mouth would be this AI speaking for it.
        let outsider = why(lines(&[("Sage", "Hello.", json!({}))]));
        assert!(outsider.contains("not in the show"), "{outsider}");
        assert!(outsider.contains("Vox and Grimble"), "{outsider}");

        // Nothing for a line to come in over: the first line of the
        // play, and a character coming in over itself -- one voice on
        // one strip cannot be two at once. Both dropped, and the show
        // goes on. Refusing was worse: a model that fills in every
        // field the schema offers could not tell from the refusal that
        // the fix was to leave the field off, and sent the same script
        // four times.
        let dropped = Script::parse(
            "The Gate",
            &lines(&[
                ("Grimble", "Hm.", json!({ "over": 1 })),
                ("Grimble", "Quite.", json!({ "over": 0.5 })),
                ("Vox", "If you say so.", json!({ "over": 0.5 })),
            ]),
            &cast,
        )
        .expect("an `over` with nothing to go over is dropped, not refused");
        assert_eq!(dropped.lines[0].over, None, "the first line of the play");
        assert_eq!(dropped.lines[1].over, None, "itself, twice running");
        assert_eq!(
            dropped.lines[2].over,
            Some(Duration::from_millis(500)),
            "and a line that really does come in over somebody keeps it"
        );

        // A line long enough to be a monologue is refused as one, and
        // told why breaking it up is better than shortening it.
        let long = "word ".repeat(LINE_MAX);
        let monologue = why(lines(&[("Grimble", &long, json!({}))]));
        assert!(monologue.contains("several lines"), "{monologue}");

        // And the shapes that aren't a script at all.
        assert!(
            Script::parse("", &lines(&[("Vox", "Hi.", json!({}))]), &cast)
                .unwrap_err()
                .contains("needs a name")
        );
        assert!(why(json!([])).contains("no lines in it"));
        assert!(why(json!("Grimble: hello")).contains("a list of lines"));
        assert!(why(lines(&[("Grimble", "", json!({}))])).contains("line 1 needs both"));
    }

    /// The whole thing: a cast, a script, and a room that hears it
    /// play out -- in order, with each line under its own name, and the
    /// interrupted one cut where it was cut.
    ///
    /// The mixer is driven in real time -- a frame is 20 ms of audio,
    /// so a tick every 20 ms is the clock a room actually runs on --
    /// because the thing under test is *when* a line goes in. Driven
    /// any faster, a pause and an overlap would both be over before
    /// the line they belong to was pushed.
    #[test]
    fn a_show_plays_out_in_order_and_nothing_is_ever_cut_off() {
        let store = Store::memory().in_room("Lobby");
        let cfg = RoomConfig {
            id: "Lobby".into(),
            title: "Lobby".into(),
            only: None,
            default_mode: Mode::Named,
            bots: vec![BotConfig::new("Vox", Vec::new())],
            reply_after: Duration::from_millis(1000),
            confirm: Duration::from_millis(300),
            engaged_for: Duration::from_millis(5000),
            history: 50,
            ai_turns: 2,
            language: "English".into(),
            languages: vec!["English".into()],
            spoken: vec!["English".into()],
        };
        let tts: crate::room::VoiceFactory =
            Arc::new(|_, _, _, _| Ok(Box::new(crate::tts::Silent)));
        let (room, _wiring) =
            Room::new(store, cfg, None, BTreeMap::new(), None, Some(tts), None).unwrap();
        for name in ["Grimble", "Pell"] {
            room.make_puppet(crate::store::Puppet {
                name: name.into(),
                about: "a character".into(),
                ..Default::default()
            })
            .unwrap();
            room.cast_puppet(name).unwrap();
        }

        // The room's own clocks, as fast as they will run: the mixer
        // plays the audio out and `tick` is what follows it into the
        // conversation.
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let clock = {
            let room = room.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                    room.mixer.tick();
                    room.tick(Instant::now());
                    std::thread::sleep(Duration::from_millis(20));
                }
            })
        };

        // The interruption is written, not performed: Grimble's line
        // stops where he was cut off, and Pell comes in over its tail.
        let script = Script::parse(
            "The Gate",
            &lines(&[
                (
                    "Grimble",
                    "None shall pass, not one of you, not today, not--",
                    json!({ "pause": 0 }),
                ),
                ("Pell", "Do be quiet.", json!({ "over": 0.4 })),
                ("Grimble", "As you like.", json!({ "pause": 0.2 })),
            ]),
            &["Vox".to_string(), "Grimble".to_string(), "Pell".to_string()],
        )
        .unwrap();
        room.begin_show("Vox", "The Gate", script.lines.len())
            .unwrap();
        let started = Instant::now();
        play(&room, &None, &script);
        let took = started.elapsed();
        room.end_show();
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        clock.join().unwrap();

        // What the room heard, in order, with the name on each line.
        let said: Vec<(String, String, bool)> = room
            .log_now()
            .into_iter()
            .filter(|e| e.who == "Grimble" || e.who == "Pell")
            .map(|e| (e.who, e.text, e.cut))
            .collect();
        assert_eq!(said.len(), 3, "three lines, each its own entry: {said:?}");
        assert_eq!(said[0].0, "Grimble");
        assert_eq!(said[1].0, "Pell");
        assert_eq!(said[2].0, "Grimble");

        // Every line whole, every line to its end. Nothing in a play
        // is ever cut off: what looks like an interruption is a line
        // the script wrote short, and it is delivered in full.
        assert!(
            said.iter().all(|(_, _, cut)| !cut),
            "nothing was cut off: {said:?}"
        );
        assert!(
            said[0].1.ends_with("not today, not--"),
            "the short line is heard out to its dash: {:?}",
            said[0]
        );
        assert!(said[1].1.starts_with("Do be quiet"), "{:?}", said[1]);
        assert!(said[2].1.starts_with("As you like"), "{:?}", said[2]);

        // And the timing was actually waited out rather than run
        // through: `Silent` speaks at 15 characters a second, so
        // Grimble's first line alone is over three seconds, Pell comes
        // in 0.4 before its end, and the last line waits 0.2 after
        // Pell. Short of that and the pauses are being skipped, which
        // is what used to happen -- `pause` was measured from the
        // moment the line before it was *pushed*, so by the time it
        // had drained the pause had already elapsed.
        assert!(
            took > Duration::from_millis(3600),
            "the play was hurried: {took:?}"
        );

        // The curtain is down, the settings are back, and the puppets
        // are still there to take questions.
        assert!(room.show_title().is_none());
        assert_eq!(room.on_stage(), ["Grimble", "Pell"]);
    }
}
