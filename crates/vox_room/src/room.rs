//! The room: who is here, the shared conversation, and whose turn it is.
//!
//! Every browser in the room sees the same state. A new tab gets a
//! snapshot ([`Room::hello`]); after that every change goes out to all of
//! them as a JSON event on one broadcast channel.
//!
//! The conversation is a list of entries. A person's entry is one
//! paragraph from their transcription engine, live while they talk. The
//! AI's entry grows only as its speech is actually played by the mixer,
//! so the log holds what was said, not what was queued to be said.
//!
//! Turn taking, run from [`Room::tick`] every 50 ms:
//!
//! * **Handing over**: once nobody has said anything for `reply_after`,
//!   the paragraphs since the last hand-over join the conversation (each
//!   engine starts a new paragraph), and the AI is asked for a reply if
//!   the room is in "always" mode or someone said its name.
//! * **Barge-in**: while the AI is replying, anyone who starts talking
//!   ducks it at once. Once their speech is confirmed (`confirm` of
//!   continuous voice, or a first recognised word) the reply is cut where
//!   the mixer had got to, and what was still queued is dropped. A cough
//!   that isn't confirmed lets the AI carry on at full level.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tracing::{error, info, warn};
use vox_transcribe::{Paragraph, Pusher};

use crate::mixer::{AiProgress, Mixer, Strip, AGENT_STRIP};
use crate::store::Store;

/// Room events waiting for a slow tab before it starts skipping.
const EVENT_CAPACITY: usize = 512;
/// Entries a new tab is sent, and kept in memory.
const LOG_LIMIT: usize = 500;
/// Shown to the model after a reply someone talked over.
const CUT_NOTE: &str = " [interrupted here]";
/// What the model is told when the room enters a game: it speaks first.
const OPENING: &str =
    "(Everyone has just entered the game. Open it: set the scene and bring the players in.)";
/// Longest game name the AI may save.
const GAME_NAME_MAX: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Human,
    Agent,
    /// A game the AI saved; the entry's text is its name.
    Game,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Human => "human",
            Kind::Agent => "agent",
            Kind::Game => "game",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "agent" => Kind::Agent,
            "game" => Kind::Game,
            _ => Kind::Human,
        }
    }
}

/// When the AI answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Only when someone says its name.
    Named,
    /// Whenever the room goes quiet.
    Always,
}

impl Mode {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "named" => Some(Mode::Named),
            "always" => Some(Mode::Always),
            _ => None,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Mode::Named => "named",
            Mode::Always => "always",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub id: String,
    pub who: String,
    pub kind: Kind,
    pub text: String,
    /// Still changing: a person still talking, or the AI still speaking.
    pub live: bool,
    /// The text includes a streaming (pass-1) guess.
    pub partial: bool,
    /// The AI was talked over here.
    pub cut: bool,
    /// Unix milliseconds when it started.
    pub at: u64,
    /// Handed to the conversation (people's entries only).
    #[serde(skip)]
    pub sent: bool,
}

#[derive(Debug, Clone)]
pub struct RoomConfig {
    pub agent_name: String,
    /// Other spellings that count as its name (speech-to-text mishears).
    pub aliases: Vec<String>,
    pub reply_after: Duration,
    pub confirm: Duration,
    /// Entries of history the model is shown.
    pub history: usize,
}

/// A turn for the agent: the conversation so far, as chat messages.
#[derive(Debug, Clone)]
pub struct Turn {
    pub generation: u64,
    pub messages: Vec<(Role, String)>,
    /// When the room handed the turn over, for the reply's timings.
    pub asked: Instant,
    /// The game being played, with its hidden prompt; None in the lobby,
    /// where the AI can design and save games.
    pub game: Option<(String, String)>,
    /// Who is in the room.
    pub people: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Default)]
struct Person {
    clients: usize,
    mic: bool,
    /// Muted their own mic. Their audio still arrives (as silence), but
    /// the room doesn't transcribe it or count it as talking.
    muted: bool,
    speaking: bool,
    speaking_since: Option<Instant>,
    /// Last time a streaming guess with words in it came in.
    last_words: Option<Instant>,
    engine: Option<Pusher>,
}

#[derive(Debug, Clone)]
struct Said {
    text: String,
    samples: usize,
    played: usize,
}

struct Reply {
    generation: u64,
    entry: String,
    sentences: Vec<Said>,
    /// The agent has sent everything it is going to say.
    complete: bool,
    /// The last words it answers, and when it was asked: for timings.
    heard: Instant,
    asked: Instant,
}

struct State {
    conversation: i64,
    /// The game this conversation plays; None is the lobby.
    game: Option<String>,
    people: BTreeMap<String, Person>,
    log: Vec<Entry>,
    mode: Mode,
    reply: Option<Reply>,
    last_change: Instant,
    /// When the last person to speak stopped.
    quiet_since: Instant,
    ducked: bool,
    /// The AI state last broadcast.
    agent_state: &'static str,
    /// A game the AI started in reply `.0`: entered once that reply has
    /// been heard (or cut off).
    starting: Option<(u64, String)>,
}

pub struct Room {
    st: Mutex<State>,
    events: broadcast::Sender<Arc<str>>,
    pub mixer: Arc<Mixer>,
    store: Mutex<Store>,
    cfg: RoomConfig,
    /// The reply the agent should be working on; anything older is dropped.
    generation: Arc<AtomicU64>,
    agent: crossbeam_channel::Sender<Turn>,
    stt: Option<EngineFactory>,
}

pub type EngineFactory = Box<dyn Fn(&str, Arc<Room>) -> anyhow::Result<Pusher> + Send + Sync>;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Room {
    pub fn new(
        store: Store,
        cfg: RoomConfig,
        generation: Arc<AtomicU64>,
        agent: crossbeam_channel::Sender<Turn>,
        stt: Option<EngineFactory>,
    ) -> anyhow::Result<Arc<Self>> {
        let conversation = store.conversation()?;
        let game = store.conversation_game(conversation)?;
        let log = store.entries(conversation, LOG_LIMIT)?;
        let mode = store
            .setting("mode")?
            .and_then(|m| Mode::parse(&m))
            .unwrap_or(Mode::Named);
        let strips: BTreeMap<String, Strip> = store
            .setting("strips")?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let people = store
            .people()?
            .into_iter()
            .map(|(name, _)| (name, Person::default()))
            .collect();
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        Ok(Arc::new(Self {
            st: Mutex::new(State {
                conversation,
                game,
                people,
                log,
                mode,
                reply: None,
                last_change: Instant::now(),
                quiet_since: Instant::now(),
                ducked: false,
                agent_state: "listening",
                starting: None,
            }),
            events,
            mixer: Mixer::new(strips),
            store: Mutex::new(store),
            cfg,
            generation,
            agent,
            stt,
        }))
    }

    pub fn config(&self) -> &RoomConfig {
        &self.cfg
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.st.lock().expect("room mutex poisoned")
    }

    fn store(&self) -> MutexGuard<'_, Store> {
        self.store.lock().expect("store mutex poisoned")
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<str>> {
        self.events.subscribe()
    }

    fn emit(&self, v: Value) {
        let _ = self.events.send(Arc::from(v.to_string()));
    }

    pub fn person_for_token(&self, token: &str) -> Option<String> {
        self.store().person_for_token(token).ok().flatten()
    }

    fn people_json(st: &State) -> Value {
        Value::Array(
            st.people
                .iter()
                .map(|(name, p)| {
                    json!({
                        "name": name,
                        "online": p.clients > 0,
                        "mic": p.mic,
                        "muted": p.muted,
                        "speaking": p.speaking,
                    })
                })
                .collect(),
        )
    }

    fn emit_people(&self, st: &State) {
        self.emit(json!({ "type": "people", "people": Self::people_json(st) }));
    }

    fn emit_entry(&self, e: &Entry) {
        self.emit(json!({ "type": "entry", "entry": e }));
    }

    /// Everything a new tab needs, for `person`.
    pub fn hello(&self, person: &str) -> Value {
        let st = self.lock();
        json!({
            "type": "hello",
            "you": person,
            "agent": { "name": self.cfg.agent_name, "state": st.agent_state, "strip": AGENT_STRIP },
            "mode": st.mode.as_str(),
            "people": Self::people_json(&st),
            "strips": self.mixer.strips(),
            "log": st.log,
            "game": st.game,
            "games": self.games(),
        })
    }

    /// A saved game and its prompt, for someone who asks to see it.
    pub fn game_with_prompt(&self, name: &str) -> Option<(crate::store::Game, String)> {
        self.store().game(name).ok().flatten()
    }

    /// The saved games, for the agent.
    pub fn list_games(&self) -> Vec<crate::store::Game> {
        self.games()
    }

    fn games(&self) -> Vec<crate::store::Game> {
        self.store().games().unwrap_or_else(|err| {
            warn!("list games: {err:#}");
            Vec::new()
        })
    }

    fn emit_games(&self) {
        self.emit(json!({ "type": "games", "games": self.games() }));
    }

    /// A tab of `person` connected.
    pub fn join(&self, person: &str) {
        let mut st = self.lock();
        st.people.entry(person.to_string()).or_default().clients += 1;
        info!(person, "joined");
        self.emit_people(&st);
    }

    /// A tab of `person` went away.
    pub fn leave(&self, person: &str) {
        let mut st = self.lock();
        let Some(p) = st.people.get_mut(person) else {
            return;
        };
        p.clients = p.clients.saturating_sub(1);
        if p.clients == 0 {
            p.mic = false;
            p.speaking = false;
            if let Some(e) = &p.engine {
                close_utterance(e);
            }
        }
        info!(person, "left");
        self.emit_people(&st);
    }

    pub fn set_mic(&self, person: &str, on: bool) {
        let mut st = self.lock();
        let Some(p) = st.people.get_mut(person) else {
            return;
        };
        if p.mic == on {
            return;
        }
        p.mic = on;
        if !on {
            p.speaking = false;
            if let Some(e) = &p.engine {
                close_utterance(e);
            }
        }
        self.emit_people(&st);
    }

    /// `person` muted or unmuted their mic.
    pub fn set_muted(&self, person: &str, muted: bool) {
        let mut st = self.lock();
        let Some(p) = st.people.get_mut(person) else {
            return;
        };
        if p.muted == muted {
            return;
        }
        p.muted = muted;
        if muted {
            p.speaking = false;
        }
        self.emit_people(&st);
    }

    /// Decoded mic audio (48 kHz mono) from `person`.
    pub fn mic_audio(self: &Arc<Self>, person: &str, pcm: &[f32]) {
        self.mixer.push_mic(person, pcm);
        // Muted (by themselves, or on the mixer for everyone): transcribe
        // silence instead, so whatever the mic picks up (the AI from their
        // speakers, say) is neither written down nor taken for talking
        // over the AI. The engine still gets audio, so its clock runs on
        // and an utterance in progress closes.
        let silent = self.mixer.is_muted(person);
        let (pusher, muted) = {
            let mut st = self.lock();
            let Some(p) = st.people.get_mut(person) else {
                return;
            };
            if p.engine.is_none() {
                if let Some(make) = &self.stt {
                    match make(person, self.clone()) {
                        Ok(e) => p.engine = Some(e),
                        Err(err) => {
                            error!(person, "no transcription: {err:#}");
                            self.error(&format!("Can't transcribe {person}: {err:#}"));
                            return;
                        }
                    }
                }
            }
            (p.engine.clone(), p.muted || silent)
        };
        if let Some(e) = pusher {
            if muted {
                e.push(&vec![0.0; pcm.len()]);
            } else {
                e.push(pcm);
            }
        }
    }

    /// A paragraph of `person`'s transcription changed.
    pub fn on_paragraph(&self, person: &str, para: &Paragraph) {
        let mut st = self.lock();
        let now = Instant::now();
        st.last_change = now;
        let partial = para.has_partial();
        if partial && !para.text.trim().is_empty() {
            if let Some(p) = st.people.get_mut(person) {
                p.last_words = Some(now);
            }
        }
        let conversation = st.conversation;
        let idx = match st.log.iter().position(|e| e.id == para.id) {
            Some(i) => i,
            None => {
                if para.text.trim().is_empty() {
                    return;
                }
                st.log.push(Entry {
                    id: para.id.clone(),
                    who: person.to_string(),
                    kind: Kind::Human,
                    text: String::new(),
                    live: true,
                    partial: true,
                    cut: false,
                    at: now_ms(),
                    sent: false,
                });
                trim_log(&mut st.log);
                st.log.len() - 1
            }
        };
        let e = &mut st.log[idx];
        e.text = para.text.clone();
        e.partial = partial;
        e.live = !e.sent;
        if e.sent {
            if let Err(err) = self.store().put_entry(conversation, e) {
                warn!("store entry: {err:#}");
            }
        }
        self.emit_entry(e);
    }

    pub fn on_paragraph_removed(&self, id: &str) {
        let mut st = self.lock();
        if let Some(i) = st
            .log
            .iter()
            .position(|e| e.id == id && e.kind == Kind::Human)
        {
            if st.log[i].sent {
                // Already part of the conversation; keep what was handed over.
                return;
            }
            st.log.remove(i);
            self.emit(json!({ "type": "entry_removed", "id": id }));
        }
    }

    /// Voice activity of `person`, about every 50 ms of their audio.
    pub fn on_level(&self, person: &str, speaking: bool) {
        let silent = self.mixer.is_muted(person);
        let mut st = self.lock();
        let Some(p) = st.people.get_mut(person) else {
            return;
        };
        // What the engine still had in hand when the mute began.
        let speaking = speaking && !p.muted && !silent;
        if p.speaking == speaking {
            return;
        }
        p.speaking = speaking;
        p.speaking_since = speaking.then(Instant::now);
        st.last_change = Instant::now();
        if !speaking {
            st.quiet_since = st.last_change;
        }
        self.emit_people(&st);
    }

    pub fn set_mode(&self, mode: &str) {
        let Some(mode) = Mode::parse(mode) else {
            return;
        };
        let mut st = self.lock();
        st.mode = mode;
        if let Err(err) = self.store().set_setting("mode", mode.as_str()) {
            warn!("store mode: {err:#}");
        }
        self.emit(json!({ "type": "mode", "mode": mode.as_str() }));
    }

    pub fn set_strip(&self, name: &str, gain: Option<f32>, mute: Option<bool>) {
        self.mixer.set_strip(name, gain, mute);
        let strips = self.mixer.strips();
        if let Ok(s) = serde_json::to_string(&strips) {
            if let Err(err) = self.store().set_setting("strips", &s) {
                warn!("store strips: {err:#}");
            }
        }
        self.emit(json!({ "type": "strips", "strips": strips }));
    }

    /// Someone pressed Stop: cut the AI off as if talked over.
    pub fn stop_agent(&self) {
        let mut st = self.lock();
        self.interrupt(&mut st);
    }

    /// Clear the log and start a fresh conversation: the game from the
    /// top when in one, else a fresh lobby.
    pub fn new_conversation(&self) {
        let mut st = self.lock();
        let game = st.game.clone();
        self.start_conversation(&mut st, game);
    }

    /// The agent starting a game the players asked for, during reply
    /// `generation`. Err says why not.
    pub fn start_game(&self, generation: u64, name: &str) -> Result<String, String> {
        let game = match self.store().game(name.trim()) {
            Ok(Some((g, _))) => g.name,
            Ok(None) => return Err(format!("there is no game called {name:?}")),
            Err(err) => return Err(format!("{err:#}")),
        };
        let mut st = self.lock();
        if self.generation.load(Ordering::SeqCst) != generation {
            return Err("the players talked over you; the game was not started".into());
        }
        info!(game, "starting game after this reply");
        if st
            .reply
            .as_ref()
            .is_some_and(|r| r.generation == generation)
        {
            st.starting = Some((generation, game.clone()));
        } else {
            self.begin_game(&mut st, game.clone());
        }
        Ok(game)
    }

    /// Reply `generation` started a game, so it has nothing more to say.
    pub fn is_starting(&self, generation: u64) -> bool {
        self.lock()
            .starting
            .as_ref()
            .is_some_and(|(g, _)| *g == generation)
    }

    /// The agent deleting a game the players asked it to, during reply
    /// `generation`; its cards leave the chat. Err says why not.
    pub fn remove_game(&self, generation: u64, name: &str) -> Result<String, String> {
        let game = match self.store().game(name.trim()) {
            Ok(Some((g, _))) => g.name,
            Ok(None) => return Err(format!("there is no game called {name:?}")),
            Err(err) => return Err(format!("{err:#}")),
        };
        let mut st = self.lock();
        if self.generation.load(Ordering::SeqCst) != generation {
            return Err("the players talked over you; the game was not deleted".into());
        }
        if let Err(err) = self.store().delete_game(&game) {
            return Err(format!("{err:#}"));
        }
        info!(game, "deleted game");
        self.remove_game_cards(&mut st, &game);
        self.emit_games();
        Ok(game)
    }

    /// Everyone into a fresh conversation playing `name`.
    pub fn enter_game(&self, name: &str) {
        let game = match self.store().game(name) {
            Ok(Some((g, _))) => g.name,
            Ok(None) => {
                self.error(&format!("There's no game called {name:?} any more."));
                return;
            }
            Err(err) => {
                self.error(&format!("Can't open {name:?}: {err:#}"));
                return;
            }
        };
        let mut st = self.lock();
        self.begin_game(&mut st, game);
    }

    fn begin_game(&self, st: &mut State, game: String) {
        info!(game, "entering game");
        if let Err(err) = self.store().mark_played(&game) {
            warn!("mark played: {err:#}");
        }
        self.emit_games();
        self.start_conversation(st, Some(game));
    }

    /// Everyone out of the game, into a fresh lobby.
    pub fn lobby(&self) {
        let mut st = self.lock();
        self.start_conversation(&mut st, None);
    }

    pub fn delete_game(&self, name: &str) {
        if self
            .lock()
            .game
            .as_deref()
            .is_some_and(|g| g.eq_ignore_ascii_case(name))
        {
            self.error(&format!(
                "{name} is being played: go back to the lobby first."
            ));
            return;
        }
        match self.store().delete_game(name) {
            Ok(true) => info!(game = name, "deleted game"),
            Ok(false) => {}
            Err(err) => self.error(&format!("Can't delete {name:?}: {err:#}")),
        }
        self.emit_games();
    }

    fn start_conversation(&self, st: &mut State, game: Option<String>) {
        st.starting = None;
        self.cut_reply(st);
        match self.store().new_conversation(game.as_deref()) {
            Ok(c) => st.conversation = c,
            Err(err) => {
                self.error(&format!("Can't start a new conversation: {err:#}"));
                return;
            }
        }
        st.log.clear();
        st.game = game;
        for p in st.people.values() {
            if let Some(e) = &p.engine {
                e.break_paragraph();
            }
        }
        // Anything said before the switch belongs to the old conversation.
        st.last_change = Instant::now();
        self.emit(json!({ "type": "reset", "log": [], "game": st.game }));
        if st.game.is_some() {
            self.ask(st, Instant::now());
        }
    }

    // -- games, for the agent -----------------------------------------------

    /// Whether `name` could be saved as a new game. Err says why not.
    pub fn check_game_name(&self, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("a game needs a name".into());
        }
        if name.chars().count() > GAME_NAME_MAX {
            return Err(format!("names are at most {GAME_NAME_MAX} characters"));
        }
        match self.store().game(name) {
            Ok(Some((taken, _))) => Err(format!("there is already a game called {:?}", taken.name)),
            Ok(None) => Ok(()),
            Err(err) => Err(format!("{err:#}")),
        }
    }

    /// Save a game the agent designed during reply `generation`, and put
    /// its button in the conversation. Err says why it wasn't saved.
    pub fn save_game(
        &self,
        generation: u64,
        name: &str,
        summary: &str,
        prompt: &str,
    ) -> Result<(), String> {
        let name = name.trim();
        self.check_game_name(name)?;
        if prompt.trim().is_empty() {
            return Err("the game needs a prompt".into());
        }
        let mut st = self.lock();
        if self.generation.load(Ordering::SeqCst) != generation {
            return Err("the players talked over you; the game was not saved".into());
        }
        match self
            .store()
            .save_game(name, summary.trim(), prompt.trim(), now_ms())
        {
            Ok(true) => {}
            Ok(false) => return Err(format!("there is already a game called {name:?}")),
            Err(err) => return Err(format!("{err:#}")),
        }
        info!(game = name, "saved game");
        self.emit_games();
        self.add_game_card(&mut st, name);
        Ok(())
    }

    /// The summary and hidden prompt of a game, so the agent can revise it.
    pub fn read_game(&self, name: &str) -> Result<(String, String), String> {
        match self.store().game(name.trim()) {
            Ok(Some((g, prompt))) => Ok((g.summary, prompt)),
            Ok(None) => Err(format!("there is no game called {name:?}")),
            Err(err) => Err(format!("{err:#}")),
        }
    }

    /// Replace a game, possibly renaming it. Its old card leaves the
    /// conversation and a new one comes in.
    pub fn revise_game(
        &self,
        generation: u64,
        name: &str,
        new_name: Option<&str>,
        summary: &str,
        prompt: &str,
    ) -> Result<String, String> {
        self.read_game(name)?;
        let old_name = match self.store().game(name.trim()) {
            Ok(Some((g, _))) => g.name,
            _ => return Err(format!("there is no game called {name:?}")),
        };
        let new_name = new_name
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or(&old_name)
            .to_string();
        if !new_name.eq_ignore_ascii_case(&old_name) {
            self.check_game_name(&new_name)?;
        } else if new_name.chars().count() > GAME_NAME_MAX {
            return Err(format!("names are at most {GAME_NAME_MAX} characters"));
        }
        if prompt.trim().is_empty() {
            return Err("the game needs a prompt".into());
        }
        let mut st = self.lock();
        if self.generation.load(Ordering::SeqCst) != generation {
            return Err("the players talked over you; the game was not changed".into());
        }
        match self
            .store()
            .update_game(&old_name, &new_name, summary.trim(), prompt.trim())
        {
            Ok(true) => {}
            Ok(false) => return Err(format!("there is no game called {name:?}")),
            Err(err) => return Err(format!("{err:#}")),
        }
        info!(game = old_name, new = new_name, "revised game");
        self.remove_game_cards(&mut st, &old_name);
        self.emit_games();
        self.add_game_card(&mut st, &new_name);
        Ok(new_name)
    }

    /// Take a game's cards out of the conversation.
    fn remove_game_cards(&self, st: &mut State, name: &str) {
        let conversation = st.conversation;
        let old_cards: Vec<String> = st
            .log
            .iter()
            .filter(|e| e.kind == Kind::Game && e.text.eq_ignore_ascii_case(name))
            .map(|e| e.id.clone())
            .collect();
        for id in &old_cards {
            if let Err(err) = self.store().delete_entry(conversation, id) {
                warn!("delete entry: {err:#}");
            }
            self.emit(json!({ "type": "entry_removed", "id": id }));
        }
        st.log.retain(|e| !old_cards.contains(&e.id));
    }

    /// A game's button, at the end of the conversation.
    fn add_game_card(&self, st: &mut State, name: &str) {
        let entry = Entry {
            id: format!("game-{}", uuid::Uuid::new_v4().simple()),
            who: self.cfg.agent_name.clone(),
            kind: Kind::Game,
            text: name.to_string(),
            live: false,
            partial: false,
            cut: false,
            at: now_ms(),
            sent: true,
        };
        if let Err(err) = self.store().put_entry(st.conversation, &entry) {
            warn!("store entry: {err:#}");
        }
        self.emit_entry(&entry);
        st.log.push(entry);
        trim_log(&mut st.log);
    }

    /// A notice that stays on everyone's screen until dismissed.
    pub fn error(&self, text: &str) {
        self.emit(json!({ "type": "notice", "level": "error", "text": text }));
    }

    pub fn emit_levels(&self) {
        let levels = self.mixer.take_levels();
        if !levels.is_empty() {
            self.emit(json!({ "type": "levels", "levels": levels }));
        }
    }

    // -- the agent's side ---------------------------------------------------

    /// One sentence of the reply `generation`, synthesized (48 kHz mono).
    /// Returns false when that reply has been dropped.
    pub fn agent_speak(&self, generation: u64, text: &str, samples: Vec<f32>) -> bool {
        let mut st = self.lock();
        let Some(r) = st.reply.as_mut().filter(|r| r.generation == generation) else {
            return false;
        };
        let index = r.sentences.len();
        r.sentences.push(Said {
            text: text.to_string(),
            samples: samples.len(),
            played: 0,
        });
        self.mixer.ai_push(generation, index, samples);
        true
    }

    /// The reply `generation` has nothing more to say.
    pub fn agent_done(&self, generation: u64) {
        let mut st = self.lock();
        if let Some(r) = st.reply.as_mut().filter(|r| r.generation == generation) {
            r.complete = true;
        }
    }

    pub fn agent_failed(&self, generation: u64, error: &str) {
        error!(generation, "reply failed: {error}");
        self.error(&format!("{} couldn't answer: {error}", self.cfg.agent_name));
        self.agent_done(generation);
    }

    // -- turn taking --------------------------------------------------------

    pub fn tick(&self, now: Instant) {
        let mut st = self.lock();
        self.follow_reply(&mut st, None);
        self.barge_in(&mut st, now);
        self.hand_over(&mut st, now);
        let state = match &st.reply {
            None if self.will_answer(&st) => "waiting",
            None => "listening",
            Some(r) if r.sentences.iter().any(|s| s.played > 0) => "speaking",
            Some(_) => "thinking",
        };
        if state == "speaking" && st.agent_state != "speaking" {
            if let Some(r) = &st.reply {
                info!(
                    generation = r.generation,
                    after_words_ms = r.heard.elapsed().as_millis() as u64,
                    after_asking_ms = r.asked.elapsed().as_millis() as u64,
                    "reply: first audio"
                );
            }
        }
        if state != st.agent_state {
            st.agent_state = state;
            self.emit(json!({ "type": "agent", "state": state }));
        }
    }

    /// Bring the AI's entry up to what the mixer has played; finish the
    /// reply once everything has been.
    fn follow_reply(&self, st: &mut State, progress: Option<AiProgress>) {
        let Some(r) = st.reply.as_mut() else {
            return;
        };
        let p = progress.unwrap_or_else(|| self.mixer.ai_progress());
        for (g, i, n) in &p.finished {
            if *g == r.generation {
                if let Some(s) = r.sentences.get_mut(*i) {
                    s.played = *n;
                }
            }
        }
        if let Some((g, i, played, _)) = p.current {
            if g == r.generation {
                if let Some(s) = r.sentences.get_mut(i) {
                    s.played = played;
                }
            }
        }
        let text = heard(&r.sentences);
        let finished = r.complete && p.idle && r.sentences.iter().all(|s| s.played >= s.samples);
        let id = r.entry.clone();
        let conversation = st.conversation;
        let Some(e) = st.log.iter_mut().find(|e| e.id == id) else {
            return;
        };
        let changed = e.text != text;
        e.text = text;
        if finished {
            e.live = false;
        }
        if finished && e.text.is_empty() {
            st.log.retain(|e| e.id != id);
            self.emit(json!({ "type": "entry_removed", "id": id }));
        } else if changed || finished {
            if finished {
                if let Err(err) = self.store().put_entry(conversation, e) {
                    warn!("store entry: {err:#}");
                }
            }
            self.emit_entry(e);
        }
        if finished {
            let generation = st.reply.take().map(|r| r.generation);
            if let Some((g, game)) = st.starting.take() {
                if Some(g) == generation {
                    self.begin_game(st, game);
                }
            }
        }
    }

    fn barge_in(&self, st: &mut State, now: Instant) {
        if st.reply.is_none() {
            if st.ducked {
                st.ducked = false;
                self.mixer.ai_duck(false);
            }
            return;
        }
        let talkers: Vec<(&String, &Person)> =
            st.people.iter().filter(|(_, p)| p.speaking).collect();
        if talkers.is_empty() {
            if st.ducked {
                st.ducked = false;
                self.mixer.ai_duck(false);
            }
            return;
        }
        let confirmed = talkers.iter().find(|(_, p)| {
            let since = p.speaking_since.unwrap_or(now);
            now.duration_since(since) >= self.cfg.confirm
                || p.last_words.is_some_and(|t| t >= since)
        });
        if let Some((who, _)) = confirmed {
            info!(person = %who, "barge-in");
            self.interrupt(st);
        } else if !st.ducked {
            st.ducked = true;
            self.mixer.ai_duck(true);
        }
    }

    /// Cut the reply in progress where the mixer has got to.
    fn interrupt(&self, st: &mut State) {
        // A game the AI was starting still starts: the players asked.
        let starting = st.starting.take();
        self.cut_reply(st);
        if let Some((_, game)) = starting {
            self.begin_game(st, game);
        }
    }

    fn cut_reply(&self, st: &mut State) {
        if st.reply.is_none() {
            return;
        }
        self.generation.fetch_add(1, Ordering::SeqCst);
        let p = self.mixer.ai_clear();
        st.ducked = false;
        self.follow_reply(st, Some(p));
        let Some(r) = st.reply.take() else {
            return;
        };
        let text = heard(&r.sentences);
        let conversation = st.conversation;
        if text.is_empty() {
            st.log.retain(|e| e.id != r.entry);
            self.emit(json!({ "type": "entry_removed", "id": r.entry }));
            return;
        }
        if let Some(e) = st.log.iter_mut().find(|e| e.id == r.entry) {
            e.text = format!("{text} —");
            e.cut = true;
            e.live = false;
            if let Err(err) = self.store().put_entry(conversation, e) {
                warn!("store entry: {err:#}");
            }
            self.emit_entry(e);
        }
    }

    /// Hand what people said to the conversation once the room is quiet,
    /// and ask the AI to answer when it should.
    fn hand_over(&self, st: &mut State, now: Instant) {
        if st.reply.is_some()
            || st.people.values().any(|p| p.speaking)
            || now.duration_since(st.last_change) < self.cfg.reply_after
        {
            return;
        }
        let pending: Vec<usize> = st
            .log
            .iter()
            .enumerate()
            .filter(|(_, e)| e.kind == Kind::Human && !e.sent)
            .map(|(i, _)| i)
            .collect();
        if pending.is_empty() || pending.iter().any(|&i| st.log[i].partial) {
            return;
        }
        let conversation = st.conversation;
        let mut said = String::new();
        let mut who = std::collections::BTreeSet::new();
        for &i in &pending {
            let e = &mut st.log[i];
            e.sent = true;
            e.live = false;
            said.push_str(&e.text);
            said.push(' ');
            who.insert(e.who.clone());
            if let Err(err) = self.store().put_entry(conversation, e) {
                warn!("store entry: {err:#}");
            }
            self.emit_entry(e);
        }
        for name in &who {
            if let Some(e) = st.people.get(name).and_then(|p| p.engine.as_ref()) {
                e.break_paragraph();
            }
        }
        let wanted = st.mode == Mode::Always || self.named(&said);
        if wanted {
            self.ask(st, now);
        }
    }

    /// Ask the AI for a reply to the conversation so far.
    fn ask(&self, st: &mut State, now: Instant) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let mut messages = conversation_messages(&st.log, self.cfg.history);
        let game = match &st.game {
            Some(name) => match self.store().game(name) {
                Ok(Some((g, prompt))) => Some((g.name, prompt)),
                _ => {
                    self.error(&format!("The game {name:?} is gone: go back to the lobby."));
                    return;
                }
            },
            None => None,
        };
        if game.is_some() && messages.first().is_none_or(|(r, _)| *r != Role::User) {
            messages.insert(0, (Role::User, OPENING.to_string()));
        }
        let entry = Entry {
            id: format!("agent-{}", uuid::Uuid::new_v4().simple()),
            who: self.cfg.agent_name.clone(),
            kind: Kind::Agent,
            text: String::new(),
            live: true,
            partial: false,
            cut: false,
            at: now_ms(),
            sent: true,
        };
        self.emit_entry(&entry);
        st.reply = Some(Reply {
            generation,
            entry: entry.id.clone(),
            sentences: Vec::new(),
            complete: false,
            heard: st.quiet_since,
            asked: now,
        });
        st.log.push(entry);
        trim_log(&mut st.log);
        info!(
            generation,
            after_words_ms = now.duration_since(st.quiet_since).as_millis() as u64,
            after_transcript_ms = now.duration_since(st.last_change).as_millis() as u64,
            "reply: asking the agent"
        );
        if self
            .agent
            .send(Turn {
                generation,
                messages,
                asked: now,
                game,
                people: st
                    .people
                    .iter()
                    .filter(|(_, p)| p.clients > 0)
                    .map(|(n, _)| n.clone())
                    .collect(),
            })
            .is_err()
        {
            error!("agent thread is gone");
            self.error(&format!(
                "{} has stopped: restart the room",
                self.cfg.agent_name
            ));
        }
    }

    /// Everyone has stopped talking and what they said since the last turn
    /// will get an answer: the room is only waiting out the pause.
    fn will_answer(&self, st: &State) -> bool {
        let mut pending = st
            .log
            .iter()
            .filter(|e| e.kind == Kind::Human && !e.sent && !e.text.is_empty())
            .peekable();
        !st.people.values().any(|p| p.speaking)
            && pending.peek().is_some()
            && (st.mode == Mode::Always || pending.any(|e| self.named(&e.text)))
    }

    /// Someone said the AI's name (or one of its aliases).
    fn named(&self, text: &str) -> bool {
        let words = words(text);
        std::iter::once(&self.cfg.agent_name)
            .chain(self.cfg.aliases.iter())
            .map(|n| words_of(n))
            .filter(|n| !n.is_empty())
            .any(|n| words.windows(n.len()).any(|w| w == n.as_slice()))
    }
}

/// Push a second of silence so the engine closes an utterance whose
/// audio just stopped (a mic turned off, a tab closed).
fn close_utterance(e: &Pusher) {
    e.push(&vec![0.0; crate::mixer::RATE as usize]);
}

fn trim_log(log: &mut Vec<Entry>) {
    if log.len() > LOG_LIMIT {
        let extra = log.len() - LOG_LIMIT;
        log.drain(..extra);
    }
}

fn words_of(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn words(s: &str) -> Vec<String> {
    words_of(s)
}

/// The text of `sentences` as far as it has been played: whole sentences,
/// then the words of the current one that have been heard in full
/// (estimated from how much of its audio has played).
fn heard(sentences: &[Said]) -> String {
    let mut out: Vec<&str> = Vec::new();
    for s in sentences {
        if s.played >= s.samples {
            out.push(s.text.trim());
            continue;
        }
        if s.played > 0 && s.samples > 0 {
            let frac = s.played as f64 / s.samples as f64;
            let p = heard_prefix(&s.text, frac);
            if !p.is_empty() {
                out.push(p);
            }
        }
        break;
    }
    out.join(" ")
}

/// The words of `text` that end within the first `frac` of it.
fn heard_prefix(text: &str, frac: f64) -> &str {
    let text = text.trim();
    let total = text.chars().count() as f64;
    let reached = (total * frac).floor() as usize;
    let mut end = 0;
    let mut chars = 0;
    let mut in_word = false;
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            if in_word && chars <= reached {
                end = i;
            }
            in_word = false;
        } else {
            in_word = true;
        }
        chars += 1;
    }
    if in_word && chars <= reached {
        end = text.len();
    }
    &text[..end]
}

/// The last `history` entries of the conversation as chat messages: what
/// people said, each line marked with who said it (a run of them in one
/// user message), and the AI's replies as far as they were heard.
fn conversation_messages(log: &[Entry], history: usize) -> Vec<(Role, String)> {
    let done: Vec<&Entry> = log
        .iter()
        .filter(|e| match e.kind {
            Kind::Human => e.sent && !e.text.trim().is_empty(),
            Kind::Agent => !e.live && !e.text.trim().is_empty(),
            Kind::Game => true,
        })
        .collect();
    let start = done.len().saturating_sub(history);
    let mut out: Vec<(Role, String)> = Vec::new();
    for e in &done[start..] {
        match e.kind {
            Kind::Human => {
                let line = format!("{}: {}", e.who, e.text.trim());
                match out.last_mut() {
                    Some((Role::User, text)) => {
                        text.push('\n');
                        text.push_str(&line);
                    }
                    _ => out.push((Role::User, line)),
                }
            }
            Kind::Agent => {
                let mut text = e.text.trim().to_string();
                if e.cut {
                    text = text.trim_end_matches('—').trim_end().to_string();
                    text.push_str(CUT_NOTE);
                }
                out.push((Role::Assistant, text));
            }
            Kind::Game => {
                let note = format!(
                    "[You saved the game \"{}\"; its card is in the chat.]",
                    e.text
                );
                match out.last_mut() {
                    Some((Role::Assistant, text)) => {
                        text.push(' ');
                        text.push_str(&note);
                    }
                    _ => out.push((Role::Assistant, note)),
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use vox_transcribe::{Clip, Stage};

    fn room(mode: Mode) -> (Arc<Room>, crossbeam_channel::Receiver<Turn>) {
        let store = Store::memory();
        store.invite("Ryan").unwrap();
        store.invite("Alex").unwrap();
        store.set_setting("mode", mode.as_str()).unwrap();
        let (tx, rx) = crossbeam_channel::unbounded();
        let cfg = RoomConfig {
            agent_name: "Vox".into(),
            aliases: vec!["box".into()],
            reply_after: Duration::from_millis(1000),
            confirm: Duration::from_millis(300),
            history: 50,
        };
        let r = Room::new(store, cfg, Arc::new(AtomicU64::new(0)), tx, None).unwrap();
        (r, rx)
    }

    fn para(id: &str, text: &str, partial: bool) -> Paragraph {
        Paragraph {
            id: id.into(),
            start_ms: 0,
            end_ms: 0,
            text: text.into(),
            clips: vec![Clip {
                id: format!("{id}-c"),
                start_ms: 0,
                duration_ms: Some(1000),
                text: text.into(),
                stage: if partial {
                    Stage::Partial
                } else {
                    Stage::Final
                },
                words: vec![],
                speaker: None,
            }],
            words: vec![],
            speaker: None,
            closed: false,
            hardened: false,
            pass3_inflight: false,
            pass4: None,
        }
    }

    fn quiet(room: &Room) -> Instant {
        Instant::now() + room.cfg.reply_after + Duration::from_millis(10)
    }

    #[test]
    fn heard_keeps_whole_words_only() {
        assert_eq!(heard_prefix("Hello there friend", 0.0), "");
        assert_eq!(heard_prefix("Hello there friend", 0.3), "Hello");
        assert_eq!(heard_prefix("Hello there friend", 0.65), "Hello there");
        assert_eq!(
            heard_prefix("Hello there friend", 1.0),
            "Hello there friend"
        );
        let s = vec![
            Said {
                text: "One two.".into(),
                samples: 10,
                played: 10,
            },
            Said {
                text: "Three four five.".into(),
                samples: 100,
                played: 70,
            },
            Said {
                text: "Six.".into(),
                samples: 10,
                played: 0,
            },
        ];
        assert_eq!(heard(&s), "One two. Three four");
    }

    #[test]
    fn answers_only_when_named_in_named_mode() {
        let (r, turns) = room(Mode::Named);
        r.on_paragraph("Ryan", &para("p1", "What do you think, Alex?", false));
        r.tick(quiet(&r));
        assert!(turns.try_recv().is_err(), "not addressed");
        assert_eq!(r.lock().agent_state, "listening");
        r.on_paragraph("Alex", &para("p2", "Let's ask vox.", false));
        r.tick(quiet(&r));
        let t = turns.try_recv().expect("named");
        assert_eq!(
            t.messages,
            vec![(
                Role::User,
                "Ryan: What do you think, Alex?\nAlex: Let's ask vox.".into()
            )]
        );
    }

    #[test]
    fn aliases_count_as_the_name() {
        let (r, turns) = room(Mode::Named);
        r.on_paragraph("Ryan", &para("p1", "Hey Box, roll for initiative.", false));
        r.tick(quiet(&r));
        assert!(turns.try_recv().is_ok());
    }

    #[test]
    fn waits_for_quiet_and_for_final_text() {
        let (r, turns) = room(Mode::Always);
        r.on_paragraph("Ryan", &para("p1", "HELLO THERE", true));
        r.tick(quiet(&r));
        assert!(turns.try_recv().is_err(), "still a streaming guess");
        r.on_paragraph("Ryan", &para("p1", "Hello there.", false));
        r.tick(Instant::now());
        assert!(turns.try_recv().is_err(), "not quiet long enough");
        assert_eq!(r.lock().agent_state, "waiting", "shown as about to answer");
        r.on_level("Ryan", true);
        r.tick(quiet(&r));
        assert!(turns.try_recv().is_err(), "still talking");
        r.on_level("Ryan", false);
        r.tick(quiet(&r));
        assert!(turns.try_recv().is_ok());
    }

    /// Start a reply and have the mixer play part of it.
    fn speaking(r: &Arc<Room>, turns: &crossbeam_channel::Receiver<Turn>) -> u64 {
        r.on_paragraph("Ryan", &para("p1", "Tell me a story, Vox.", false));
        r.tick(quiet(r));
        let g = turns.try_recv().unwrap().generation;
        assert!(r.agent_speak(g, "Once upon a time there was a dragon.", vec![0.1; 48_000]));
        assert!(r.agent_speak(g, "It was very old.", vec![0.1; 48_000]));
        r.agent_done(g);
        g
    }

    fn agent_entry(r: &Room) -> Entry {
        let st = r.lock();
        st.log
            .iter()
            .rev()
            .find(|e| e.kind == Kind::Agent)
            .cloned()
            .unwrap_or_else(|| panic!("no agent entry: {:?}", st.log))
    }

    #[test]
    fn a_cut_reply_keeps_only_what_was_said() {
        let (r, turns) = room(Mode::Named);
        speaking(&r, &turns);
        // The mixer plays half of the first sentence.
        for _ in 0..25 {
            r.mixer.tick();
        }
        r.tick(Instant::now());
        assert_eq!(agent_entry(&r).text, "Once upon a time");
        r.on_level("Alex", true);
        let since = Instant::now();
        r.tick(since);
        assert!(r.lock().ducked, "ducked as soon as someone talks");
        assert!(r.lock().reply.is_some(), "not cut before it's confirmed");
        r.tick(since + Duration::from_millis(400));
        let e = agent_entry(&r);
        assert!(e.cut && !e.live);
        assert!(e.text.ends_with(" —"), "{:?}", e.text);
        assert!(r.lock().reply.is_none());
        // The model hears where it was cut off.
        let msgs = conversation_messages(&r.lock().log, 50);
        assert!(msgs.last().unwrap().1.ends_with(CUT_NOTE));
    }

    #[test]
    fn a_cough_only_ducks() {
        let (r, turns) = room(Mode::Named);
        speaking(&r, &turns);
        r.on_level("Alex", true);
        r.tick(Instant::now());
        assert!(r.lock().ducked);
        r.on_level("Alex", false);
        r.tick(Instant::now());
        assert!(!r.lock().ducked);
        assert!(r.lock().reply.is_some());
    }

    #[test]
    fn a_muted_mic_never_interrupts() {
        let (r, turns) = room(Mode::Named);
        speaking(&r, &turns);
        // Muted on the mixer: the AI coming back through their speakers.
        r.set_strip("Alex", None, Some(true));
        r.on_level("Alex", true);
        r.tick(Instant::now() + Duration::from_millis(400));
        assert!(!r.lock().ducked);
        assert!(r.lock().reply.is_some());
        // Muted by themselves.
        r.set_strip("Alex", None, Some(false));
        r.set_muted("Ryan", true);
        r.on_level("Ryan", true);
        r.tick(Instant::now() + Duration::from_millis(400));
        assert!(r.lock().reply.is_some());
        r.set_muted("Ryan", false);
        r.on_level("Ryan", true);
        r.tick(Instant::now() + Duration::from_millis(400));
        assert!(r.lock().reply.is_none(), "unmuted, they interrupt again");
    }

    #[test]
    fn a_first_word_confirms_at_once() {
        let (r, turns) = room(Mode::Named);
        speaking(&r, &turns);
        r.on_level("Alex", true);
        r.on_paragraph("Alex", &para("p9", "WAIT", true));
        r.tick(Instant::now());
        assert!(r.lock().reply.is_none());
    }

    #[test]
    fn talking_over_a_reply_not_yet_heard_drops_it() {
        let (r, turns) = room(Mode::Named);
        r.on_paragraph("Ryan", &para("p1", "Vox?", false));
        r.tick(quiet(&r));
        let g = turns.try_recv().unwrap().generation;
        r.on_level("Ryan", true);
        r.tick(Instant::now() + Duration::from_millis(400));
        assert!(r.lock().log.iter().all(|e| e.kind != Kind::Agent));
        assert!(
            !r.agent_speak(g, "Too late.", vec![0.0; 100]),
            "stale reply refused"
        );
    }

    #[test]
    fn a_reply_finishes_once_played() {
        let (r, turns) = room(Mode::Named);
        speaking(&r, &turns);
        for _ in 0..100 {
            r.mixer.tick();
        }
        r.tick(Instant::now());
        let e = agent_entry(&r);
        assert!(!e.live && !e.cut);
        assert_eq!(
            e.text,
            "Once upon a time there was a dragon. It was very old."
        );
        assert!(r.lock().reply.is_none());
    }

    fn cards(r: &Room) -> Vec<String> {
        r.lock()
            .log
            .iter()
            .filter(|e| e.kind == Kind::Game)
            .map(|e| e.text.clone())
            .collect()
    }

    #[test]
    fn a_saved_game_can_be_revised_and_played() {
        let (r, turns) = room(Mode::Always);
        let g = r.generation.load(Ordering::SeqCst);
        assert_eq!(r.check_game_name("Dragon Heist"), Ok(()));
        r.save_game(g, "Dragon Heist", "A heist.", "Run a heist.")
            .unwrap();
        assert!(
            r.check_game_name("dragon heist").is_err(),
            "names are unique"
        );
        assert!(r.save_game(g, "DRAGON HEIST", "x", "y").is_err());
        assert_eq!(cards(&r), ["Dragon Heist"]);

        // Revised (and renamed): the old card goes, a new one comes.
        assert_eq!(r.read_game("dragon heist").unwrap().1, "Run a heist.");
        let name = r
            .revise_game(
                g,
                "Dragon Heist",
                Some("Dragon Caper"),
                "A caper.",
                "Run a caper.",
            )
            .unwrap();
        assert_eq!(name, "Dragon Caper");
        assert_eq!(cards(&r), ["Dragon Caper"]);
        assert!(r.read_game("Dragon Heist").is_err());

        // The model is told about the button in later turns.
        let msgs = conversation_messages(&r.lock().log, 50);
        assert!(msgs.last().unwrap().1.contains("\"Dragon Caper\""));

        // Entering: a fresh conversation, and the AI opens with the prompt.
        r.enter_game("dragon caper");
        let t = turns.try_recv().expect("the AI opens the game");
        assert_eq!(t.game, Some(("Dragon Caper".into(), "Run a caper.".into())));
        assert_eq!(t.messages, vec![(Role::User, OPENING.to_string())]);
        assert!(cards(&r).is_empty());

        // It can't be deleted while it's being played.
        r.delete_game("Dragon Caper");
        assert_eq!(r.list_games().len(), 1);

        // Back in the lobby the AI has its tools again, and it can go.
        r.stop_agent();
        r.lobby();
        assert_eq!(r.lock().game, None);
        // Played games can still be revised in the lobby.
        let g = r.generation.load(Ordering::SeqCst);
        r.revise_game(g, "Dragon Caper", None, "A caper.", "Run a better caper.")
            .unwrap();
        assert!(r.list_games()[0].played, "still marked played");
        assert_eq!(
            r.read_game("Dragon Caper").unwrap().1,
            "Run a better caper."
        );
        r.delete_game("Dragon Caper");
        assert!(r.list_games().is_empty());
    }

    #[test]
    fn a_game_started_by_voice_opens_after_the_reply_is_heard() {
        let (r, turns) = room(Mode::Named);
        let g0 = r.generation.load(Ordering::SeqCst);
        r.save_game(g0, "Quick Draw", "A showdown.", "Run a showdown.")
            .unwrap();
        r.on_paragraph("Ryan", &para("p1", "Vox, let's play quick draw.", false));
        r.tick(quiet(&r));
        let g = turns.try_recv().unwrap().generation;
        assert!(r.agent_speak(g, "Starting Quick Draw.", vec![0.1; 48_000]));
        assert!(r.start_game(g, "quick draw").is_ok());
        assert!(r.is_starting(g));
        r.agent_done(g);
        assert_eq!(r.lock().game, None, "not until the reply is heard");
        for _ in 0..60 {
            r.mixer.tick();
        }
        r.tick(Instant::now());
        assert_eq!(r.lock().game.as_deref(), Some("Quick Draw"));
        let t = turns.try_recv().expect("the AI opens the game");
        assert_eq!(t.game.unwrap().1, "Run a showdown.");
        assert!(r.start_game(t.generation, "nope").is_err());
    }

    #[test]
    fn a_game_deleted_by_voice_leaves_the_chat() {
        let (r, _turns) = room(Mode::Named);
        let g = r.generation.load(Ordering::SeqCst);
        r.save_game(g, "Zork", "Caves.", "Dark.").unwrap();
        assert_eq!(cards(&r), ["Zork"]);
        assert_eq!(r.remove_game(g, "zork"), Ok("Zork".into()));
        assert!(cards(&r).is_empty());
        assert!(r.list_games().is_empty());
        assert!(r.remove_game(g, "zork").is_err());
    }

    #[test]
    fn a_game_isnt_saved_by_a_reply_that_was_talked_over() {
        let (r, _turns) = room(Mode::Always);
        let g = r.generation.load(Ordering::SeqCst);
        r.generation.fetch_add(1, Ordering::SeqCst);
        assert!(r.save_game(g, "Zork", "Caves.", "Dark.").is_err());
        assert!(r.list_games().is_empty());
    }
}
