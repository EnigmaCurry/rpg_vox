//! The room: who is here, the shared conversation, and whose turn it is.
//!
//! Every browser in the room sees the same state. A new tab gets a
//! snapshot ([`Room::hello`]); after that every change goes out to all of
//! them as a JSON event on one broadcast channel. An [`Event`] may name
//! one person, and then only their own tabs are sent it: that is how a
//! card the AI dealt reaches nobody else's browser.
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
//! * **Staying in**: in `Named` mode, addressing the AI by name puts the
//!   room in a back-and-forth -- it answers every pause from then on,
//!   without being named again, until the room has been idle for
//!   `engaged_for` (or the AI itself bows out).
//! * **Barge-in**: while the AI is replying, anyone who starts talking
//!   ducks it at once. Once their speech is confirmed (`confirm` of
//!   continuous voice, or a first recognised word) the reply is cut where
//!   the mixer had got to, and what was still queued is dropped. A cough
//!   that isn't confirmed lets the AI carry on at full level.

use std::collections::{btree_map, BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tracing::{error, info, warn};
use vox_transcribe::{Paragraph, ParagraphMode, Pusher};

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
/// Longest title on a card the AI deals.
const CARD_TITLE_MAX: usize = 60;
/// Longest body on a card: it is read on screen, not aloud, and a card
/// shares its column with everything else someone is holding.
const CARD_BODY_MAX: usize = 2000;
/// Cards one person may be holding in a conversation at once.
const CARDS_MAX: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Human,
    Agent,
    /// A game the AI saved; the entry's text is its name.
    Game,
    /// A card the AI posted for the whole room: the entry's title and
    /// text are the card's. It stays in the conversation like anything
    /// that was said.
    Card,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Human => "human",
            Kind::Agent => "agent",
            Kind::Game => "game",
            Kind::Card => "card",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "agent" => Kind::Agent,
            "game" => Kind::Game,
            "card" => Kind::Card,
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
    /// At every pause the AI is given the turn and decides for itself
    /// whether this is its moment, so three people can talk with it in
    /// the room without it answering every gap.
    Auto,
    /// Whenever the room goes quiet, no judgment involved.
    Always,
}

impl Mode {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "named" => Some(Mode::Named),
            "auto" => Some(Mode::Auto),
            "always" => Some(Mode::Always),
            _ => None,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Mode::Named => "named",
            Mode::Auto => "auto",
            Mode::Always => "always",
        }
    }
}

/// Why the AI has been given the turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    /// Say something: it was named, it is opening a game, or the room
    /// answers every pause.
    Speak,
    /// Decide first whether this is its moment at all, and stay quiet if
    /// it isn't. `lull` is how many seconds the room has been quiet, on
    /// the second look at a silence the AI asked to be shown again.
    Decide { lull: Option<u64> },
}

impl Cue {
    pub fn deciding(self) -> bool {
        matches!(self, Cue::Decide { .. })
    }
}

/// How the AI takes its turn in a game.
///
/// A game is run by its own rules, so it may need the AI answering every
/// pause (it is dealing the turns) or never unless called. `Room` leaves
/// it to whatever the people picked, back-and-forth and all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplyMode {
    #[default]
    Room,
    Auto,
    Always,
    Named,
}

impl ReplyMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "room" | "" => Some(ReplyMode::Room),
            "auto" => Some(ReplyMode::Auto),
            "always" => Some(ReplyMode::Always),
            "named" => Some(ReplyMode::Named),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ReplyMode::Room => "room",
            ReplyMode::Auto => "auto",
            ReplyMode::Always => "always",
            ReplyMode::Named => "named",
        }
    }

    /// The mode this requires of the room, for as long as the game is
    /// being played; `None` leaves the room's own mode alone.
    fn forces(self) -> Option<Mode> {
        match self {
            ReplyMode::Room => None,
            ReplyMode::Auto => Some(Mode::Auto),
            ReplyMode::Always => Some(Mode::Always),
            ReplyMode::Named => Some(Mode::Named),
        }
    }
}

/// What a saved game is, besides its prompt: the parts the room acts on
/// rather than the model. Unknown and missing fields default, so a game
/// saved before a setting existed still loads.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameSettings {
    /// How the AI takes its turn while this game is being played.
    pub reply: ReplyMode,
    /// How many players the game is for.
    pub players: Players,
}

/// How many people a game is designed for. Either end may be open: a
/// game for three or more, or for up to six, or for any number at all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Players {
    pub min: Option<u32>,
    pub max: Option<u32>,
}

/// More players than any game here is for; past this it is a typo.
const PLAYERS_MAX: u32 = 99;

impl Players {
    /// Err says what is wrong with the pair.
    pub fn check(self) -> Result<Self, String> {
        if let Some(n) = self.min.or(self.max) {
            if n == 0 {
                return Err("a game needs at least one player".into());
            }
        }
        if self.min.unwrap_or(0) > PLAYERS_MAX || self.max.unwrap_or(0) > PLAYERS_MAX {
            return Err(format!("at most {PLAYERS_MAX} players"));
        }
        match (self.min, self.max) {
            (Some(lo), Some(hi)) if lo > hi => Err(format!(
                "players_min ({lo}) is more than players_max ({hi})"
            )),
            _ => Ok(self),
        }
    }

    /// How it reads, for the model that is revising it.
    pub fn describe(self) -> String {
        match (self.min, self.max) {
            (Some(lo), Some(hi)) if lo == hi => format!("{lo}"),
            (Some(lo), Some(hi)) => format!("{lo} to {hi}"),
            (Some(lo), None) => format!("{lo} or more"),
            (None, Some(hi)) => format!("up to {hi}"),
            (None, None) => "any number".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub id: String,
    pub who: String,
    pub kind: Kind,
    /// A posted card's heading; empty for everything that was said.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub title: String,
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

/// A card the AI dealt to one person, shown among their own cards and
/// nobody else's. It belongs to the conversation it was dealt in.
#[derive(Debug, Clone, Serialize)]
pub struct Card {
    pub id: String,
    /// The one person who ever sees it.
    pub to: String,
    pub title: String,
    pub body: String,
    /// Unix milliseconds when it was dealt.
    pub at: u64,
}

/// One room event, and who may see it.
///
/// `to` is `None` for everything the whole room shares, and `Some(name)`
/// for an event only that person's tabs are sent -- the filtering is here
/// on the server, so a secret never reaches another browser at all.
#[derive(Debug, Clone)]
pub struct Event {
    pub to: Option<Arc<str>>,
    pub json: Arc<str>,
}

impl Event {
    pub fn is_for(&self, person: &str) -> bool {
        self.to.as_deref().is_none_or(|to| to == person)
    }
}

#[derive(Debug, Clone)]
pub struct RoomConfig {
    pub agent_name: String,
    /// Other spellings that count as its name (speech-to-text mishears).
    pub aliases: Vec<String>,
    pub reply_after: Duration,
    pub confirm: Duration,
    /// How long the room may be idle before a back-and-forth lapses and
    /// the AI goes back to waiting for its name.
    pub engaged_for: Duration,
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
    /// A back-and-forth is on: the AI has the turn at every pause
    /// because it was named, not because the room is set that way.
    pub engaged: bool,
    /// Whether the AI is to speak, or to decide whether to speak at all.
    pub cue: Cue,
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
    /// Push to talk: unmuted means holding the key, and everything said
    /// until release is one entry, however long the pauses.
    ptt: bool,
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
    /// The AI is deciding whether to speak at all. Until it says
    /// something its entry is held back, so a turn it passes on leaves
    /// nothing on anyone's screen.
    cue: Cue,
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
    /// The mode that game requires, if it requires one.
    game_mode: Option<Mode>,
    people: BTreeMap<String, Person>,
    log: Vec<Entry>,
    /// What the people picked, and what a restart comes back to.
    chosen: Mode,
    /// What the room is actually doing: `chosen`, unless the game being
    /// played requires a mode, or a back-and-forth is on.
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
    /// In a back-and-forth: `Named` mode was flipped to `Always` because
    /// someone said the AI's name, and goes back once the room has been
    /// idle for `engaged_for`. The instant is the last thing that kept it
    /// going. `None` is the room's own mode, as chosen.
    engaged: Option<Instant>,
    /// The AI passed on its turn but asked to be shown the room again if
    /// the quiet lasted: when to look, and how long that is in seconds.
    reconsider: Option<(Instant, u64)>,
}

pub struct Room {
    st: Mutex<State>,
    events: broadcast::Sender<Event>,
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
        let chosen = store
            .setting("mode")?
            .and_then(|m| Mode::parse(&m))
            .unwrap_or(Mode::Named);
        // The game the room was left playing may require its own mode.
        let game_mode = game
            .as_deref()
            .and_then(|g| store.game(g).ok().flatten())
            .and_then(|(g, _)| g.settings.reply.forces());
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
                game_mode,
                people,
                log,
                chosen,
                mode: game_mode.unwrap_or(chosen),
                reply: None,
                last_change: Instant::now(),
                quiet_since: Instant::now(),
                ducked: false,
                agent_state: "listening",
                starting: None,
                engaged: None,
                reconsider: None,
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

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// To the whole room.
    fn emit(&self, v: Value) {
        let _ = self.events.send(Event {
            to: None,
            json: Arc::from(v.to_string()),
        });
    }

    /// To one person's tabs only. Nobody else's browser is sent it.
    fn emit_to(&self, person: &str, v: Value) {
        let _ = self.events.send(Event {
            to: Some(Arc::from(person)),
            json: Arc::from(v.to_string()),
        });
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

    /// Bring the roster back in line with who is actually invited.
    ///
    /// `vox_room invite` and `uninvite` run in their own process and write
    /// straight to the database, so the running room only learns about
    /// them by looking. Someone newly invited appears, and someone
    /// uninvited disappears once their last tab has gone -- while they are
    /// still connected they stay on the roster, because they are still in
    /// the room and still being heard.
    pub fn reconcile_people(&self) {
        let mut st = self.lock();
        // State first, then the store: the order everything else here uses.
        let invited: BTreeSet<String> = match self.store().people() {
            Ok(people) => people.into_iter().map(|(name, _)| name).collect(),
            Err(err) => {
                warn!("couldn't read the invite list: {err:#}");
                return;
            }
        };
        let before = st.people.len();
        st.people
            .retain(|name, p| p.clients > 0 || invited.contains(name));
        let mut changed = st.people.len() != before;
        for name in invited {
            if let btree_map::Entry::Vacant(slot) = st.people.entry(name) {
                info!(person = %slot.key(), "invited");
                slot.insert(Person::default());
                changed = true;
            }
        }
        if changed {
            self.emit_people(&st);
        }
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
            "engaged": st.engaged.is_some(),
            "locked": st.game_mode.is_some(),
            "people": Self::people_json(&st),
            "strips": self.mixer.strips(),
            "log": st.log,
            "game": st.game,
            "games": self.games(),
            "cards": self.cards_for(&st, person),
        })
    }

    // -- cards: what the AI tells one person alone -------------------------

    /// The cards `person` is holding in this conversation.
    fn cards_for(&self, st: &State, person: &str) -> Vec<Card> {
        self.store()
            .cards(st.conversation, person)
            .unwrap_or_else(|err| {
                warn!("read cards: {err:#}");
                Vec::new()
            })
    }

    /// Everyone on the roster, for the agent: the name to address a card
    /// to, whether they are here, and what they are already holding.
    pub fn people_here(&self) -> Vec<Value> {
        let st = self.lock();
        let mut holding: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (who, title) in self
            .store()
            .card_titles(st.conversation)
            .unwrap_or_else(|err| {
                warn!("read cards: {err:#}");
                Vec::new()
            })
        {
            holding.entry(who).or_default().push(title);
        }
        st.people
            .iter()
            .map(|(name, p)| {
                json!({
                    "name": name,
                    "here": p.clients > 0,
                    // Being heard: a mic open and not muted.
                    "listening": p.mic && !p.muted,
                    // The titles of what they are holding, which is
                    // what a card is addressed by.
                    "cards": holding.get(name).cloned().unwrap_or_default(),
                })
            })
            .collect()
    }

    /// Deal a card to one person: it joins their own cards and nobody
    /// else's. Err says why it wasn't dealt.
    ///
    /// Unlike saving a game, this goes through even if the players talked
    /// over the reply that called it: the card is the point, and losing
    /// one silently is worse than one arriving after an interruption.
    pub fn send_card(&self, to: &str, title: &str, body: &str) -> Result<String, String> {
        let (title, body) = (title.trim(), body.trim());
        if title.is_empty() || body.is_empty() {
            return Err("a card needs a title and a body".into());
        }
        if title.chars().count() > CARD_TITLE_MAX {
            return Err(format!("titles are at most {CARD_TITLE_MAX} characters"));
        }
        if body.chars().count() > CARD_BODY_MAX {
            return Err(format!("a card is at most {CARD_BODY_MAX} characters"));
        }
        let st = self.lock();
        // Speech-to-text mangles names, so match the roster loosely; the
        // error names everyone, so the model can pick again.
        let Some(to) = st
            .people
            .keys()
            .find(|n| n.eq_ignore_ascii_case(to.trim()))
            .cloned()
        else {
            let who: Vec<&str> = st.people.keys().map(String::as_str).collect();
            return Err(format!(
                "there is nobody called {to:?} in the room; it is {}",
                if who.is_empty() {
                    "empty".to_string()
                } else {
                    who.join(", ")
                }
            ));
        };
        // Dealing a title they already hold rewrites that card where it
        // sits, rather than stacking another copy beside it.
        let existing = self
            .store()
            .card_titled(st.conversation, &to, title)
            .unwrap_or_else(|err| {
                warn!("read cards: {err:#}");
                None
            });
        let held = self.cards_for(&st, &to).len();
        if existing.is_none() && held >= CARDS_MAX {
            return Err(format!(
                "{to} is already holding {held} cards; take some back first"
            ));
        }
        let replaced = existing.is_some();
        let card = Card {
            id: existing.map_or_else(
                || format!("card-{}", uuid::Uuid::new_v4().simple()),
                |c| c.id,
            ),
            to: to.clone(),
            title: title.to_string(),
            body: body.to_string(),
            at: now_ms(),
        };
        let wrote = {
            let store = self.store();
            if replaced {
                store.update_card(st.conversation, &card)
            } else {
                store.put_card(st.conversation, &card)
            }
        };
        if let Err(err) = wrote {
            return Err(format!("{err:#}"));
        }
        info!(to = %to, title = %card.title, replaced, "dealt a card");
        // Only their tabs; the body never reaches another browser.
        self.emit_to(
            &to,
            json!({ "type": "card", "card": card, "replaced": replaced }),
        );
        Ok(to)
    }

    /// Take a card back: it leaves that person's screen at once. The
    /// cards are the AI's to give and to withdraw, and nobody in the
    /// room can clear one themselves. Err says there was no such card.
    pub fn take_back_card(&self, to: &str, title: &str) -> Result<String, String> {
        let st = self.lock();
        let Some(to) = st
            .people
            .keys()
            .find(|n| n.eq_ignore_ascii_case(to.trim()))
            .cloned()
        else {
            return Err(format!("there is nobody called {to:?} in the room"));
        };
        let card = self
            .store()
            .card_titled(st.conversation, &to, title)
            .map_err(|err| format!("{err:#}"))?;
        let Some(card) = card else {
            let held = self.cards_for(&st, &to);
            return Err(format!(
                "{to} isn't holding a card called {title:?}; they have {}",
                if held.is_empty() {
                    "none at all".to_string()
                } else {
                    held.iter()
                        .map(|c| format!("{:?}", c.title))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ));
        };
        if let Err(err) = self.store().delete_card(st.conversation, &card.id) {
            return Err(format!("{err:#}"));
        }
        info!(to = %to, title = %card.title, "took a card back");
        self.emit_to(&to, json!({ "type": "card_removed", "id": card.id }));
        Ok(card.title)
    }

    /// Post a card to the whole room: it goes into the conversation
    /// where everyone can read it, and stays there. Err says why not.
    ///
    /// Unlike a card dealt to one person there is no taking it back: it
    /// is part of
    /// what the room has been told, like anything that was said.
    pub fn post_card(&self, title: &str, body: &str) -> Result<String, String> {
        let (title, body) = (title.trim(), body.trim());
        if title.is_empty() || body.is_empty() {
            return Err("a card needs a title and a body".into());
        }
        if title.chars().count() > CARD_TITLE_MAX {
            return Err(format!("titles are at most {CARD_TITLE_MAX} characters"));
        }
        if body.chars().count() > CARD_BODY_MAX {
            return Err(format!("a card is at most {CARD_BODY_MAX} characters"));
        }
        let mut st = self.lock();
        let entry = Entry {
            id: format!("card-{}", uuid::Uuid::new_v4().simple()),
            who: self.cfg.agent_name.clone(),
            kind: Kind::Card,
            title: title.to_string(),
            text: body.to_string(),
            live: false,
            partial: false,
            cut: false,
            at: now_ms(),
            sent: true,
        };
        if let Err(err) = self.store().put_entry(st.conversation, &entry) {
            return Err(format!("{err:#}"));
        }
        info!(title, "posted a card to the room");
        self.emit_entry(&entry);
        st.log.push(entry);
        trim_log(&mut st.log);
        Ok(title.to_string())
    }

    /// What people are holding, for the AI: one person's cards, or
    /// everyone's. The AI wrote them, but a long game outruns what it
    /// can still see of its own conversation.
    pub fn read_cards(&self, to: Option<&str>) -> Vec<Value> {
        let st = self.lock();
        let names: Vec<String> = match to.map(str::trim).filter(|t| !t.is_empty()) {
            Some(one) => st
                .people
                .keys()
                .filter(|n| n.eq_ignore_ascii_case(one))
                .cloned()
                .collect(),
            None => st.people.keys().cloned().collect(),
        };
        names
            .into_iter()
            .map(|name| {
                let cards: Vec<Value> = self
                    .cards_for(&st, &name)
                    .into_iter()
                    .map(|c| json!({ "title": c.title, "body": c.body }))
                    .collect();
                json!({ "name": name, "cards": cards })
            })
            .collect()
    }

    // -- the AI's own turn taking ------------------------------------------

    /// The longest silence the AI may ask to be shown again after.
    const LULL_MAX: u64 = 120;

    /// The AI passing on its turn: it decided this pause wasn't its
    /// moment, so nothing is said and nothing appears on screen.
    ///
    /// `ask_again_in` has the room give it the turn once more if the
    /// quiet lasts that many seconds -- how it waits for a real silence
    /// rather than the gap it was just shown. Only once per silence: a
    /// second look that passes again is the end of it, so this can't
    /// turn into a loop while nobody is saying anything.
    pub fn stay_quiet(&self, generation: u64, ask_again_in: Option<u64>) -> Result<(), String> {
        let mut st = self.lock();
        let Some(r) = st.reply.as_ref().filter(|r| r.generation == generation) else {
            return Err("that turn is over; the room has moved on".into());
        };
        if !r.cue.deciding() {
            return Err("you were asked to answer, not whether to answer".into());
        }
        let again = match r.cue {
            // A second look that passes is the end of this silence.
            Cue::Decide { lull: Some(_) } => None,
            _ => ask_again_in
                .filter(|s| *s > 0)
                .map(|s| s.min(Self::LULL_MAX)),
        };
        self.cut_reply(&mut st);
        st.reconsider = again.map(|s| (Instant::now() + Duration::from_secs(s), s));
        info!(generation, again, "reply: stayed quiet");
        Ok(())
    }

    /// Give the AI the turn again once a silence it asked about has
    /// actually lasted.
    fn reconsider(&self, st: &mut State, now: Instant) {
        let Some((at, lull)) = st.reconsider else {
            return;
        };
        if now < at {
            return;
        }
        st.reconsider = None;
        // Only into a room that is still quiet: anyone talking, or
        // anything waiting to be handed over, means the silence is over
        // and hand_over is the one to decide.
        if st.reply.is_some()
            || st.people.values().any(|p| p.speaking || p.holding())
            || st.log.iter().any(|e| e.kind == Kind::Human && !e.sent)
        {
            return;
        }
        self.ask(st, now, Cue::Decide { lull: Some(lull) });
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
            // Let go of push to talk: what they said is done.
            if p.ptt {
                if let Some(e) = &p.engine {
                    e.break_paragraph();
                }
            }
        }
        self.emit_people(&st);
    }

    /// `person` turned push to talk on or off.
    pub fn set_ptt(&self, person: &str, on: bool) {
        let mut st = self.lock();
        let Some(p) = st.people.get_mut(person) else {
            return;
        };
        p.ptt = on;
        if let Some(e) = &p.engine {
            e.set_paragraph_mode(paragraph_mode(on));
        }
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
                        Ok(e) => {
                            e.set_paragraph_mode(paragraph_mode(p.ptt));
                            p.engine = Some(e);
                        }
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
                    title: String::new(),
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

    /// Someone picked a mode: that is the room's own, and it ends any
    /// back-and-forth. The game being played may still require another.
    pub fn set_mode(&self, mode: &str) {
        let Some(mode) = Mode::parse(mode) else {
            return;
        };
        let mut st = self.lock();
        st.chosen = mode;
        st.engaged = None;
        if let Err(err) = self.store().set_setting("mode", mode.as_str()) {
            warn!("store mode: {err:#}");
        }
        self.refresh_mode(&mut st);
    }

    /// The AI standing down by itself: back to answering only when it is
    /// named. It changes the room's own mode, as if someone picked it,
    /// and answers with what the room is actually doing afterwards --
    /// which is still `always` if the game being played requires it.
    pub fn wait_until_named(&self) -> &'static str {
        let mut st = self.lock();
        if st.chosen != Mode::Named || st.engaged.is_some() {
            info!("the AI stood down: answering only when named again");
        }
        st.chosen = Mode::Named;
        st.engaged = None;
        if let Err(err) = self.store().set_setting("mode", Mode::Named.as_str()) {
            warn!("store mode: {err:#}");
        }
        self.refresh_mode(&mut st);
        st.mode.as_str()
    }

    /// Work out what the room should be doing and tell every tab.
    ///
    /// A back-and-forth wins over the game being played, which wins over
    /// what the people picked. Only the last of those is stored, so a
    /// restart comes back to what was actually chosen.
    fn refresh_mode(&self, st: &mut State) {
        st.mode = if st.engaged.is_some() {
            // It keeps the turn, but it still judges each pause: the
            // people may well have turned to each other.
            Mode::Auto
        } else {
            st.game_mode.unwrap_or(st.chosen)
        };
        self.emit(json!({
            "type": "mode",
            "mode": st.mode.as_str(),
            "engaged": st.engaged.is_some(),
            // The game requires this one: the buttons are not the room's
            // to change while it is being played.
            "locked": st.game_mode.is_some(),
        }));
    }

    /// Someone said the AI's name in `Named` mode: it keeps the turn at
    /// every pause from now on, so they can talk back and forth without
    /// saying it again, until the room has been idle for `engaged_for`.
    fn engage(&self, st: &mut State, now: Instant) {
        info!(
            for_ms = self.cfg.engaged_for.as_millis() as u64,
            "named: it keeps the turn at every pause until the room goes quiet"
        );
        st.engaged = Some(now);
        self.refresh_mode(st);
    }

    /// Keep a back-and-forth going while anything is happening, and end
    /// it once the room has been idle long enough.
    fn follow_engagement(&self, st: &mut State, now: Instant) {
        let Some(since) = st.engaged else {
            return;
        };
        // Someone talking, or the AI answering: the countdown only starts
        // once the room actually falls quiet.
        if st.reply.is_some() || st.people.values().any(|p| p.speaking || p.holding()) {
            st.engaged = Some(now);
            return;
        }
        if now.duration_since(since) < self.cfg.engaged_for {
            return;
        }
        info!(
            quiet_ms = now.duration_since(since).as_millis() as u64,
            "quiet: back to answering only when named"
        );
        st.engaged = None;
        self.refresh_mode(st);
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
        st.reconsider = None;
        self.cut_reply(st);
        match self.store().new_conversation(game.as_deref()) {
            Ok(c) => st.conversation = c,
            Err(err) => {
                self.error(&format!("Can't start a new conversation: {err:#}"));
                return;
            }
        }
        st.log.clear();
        // A game may require its own mode for as long as it is played;
        // leaving it puts the room back the way the people had it.
        st.game_mode = game
            .as_deref()
            .and_then(|g| self.store().game(g).ok().flatten())
            .and_then(|(g, _)| g.settings.reply.forces());
        st.engaged = None;
        st.game = game;
        self.refresh_mode(st);
        for p in st.people.values() {
            if let Some(e) = &p.engine {
                e.break_paragraph();
            }
        }
        // Anything said before the switch belongs to the old conversation.
        st.last_change = Instant::now();
        // Cards belong to the conversation they were dealt in, so a fresh
        // one leaves everyone holding nothing.
        self.emit(json!({ "type": "reset", "log": [], "cards": [], "game": st.game }));
        if st.game.is_some() {
            // A game opens with the AI setting the scene, whatever the
            // room's mode: nobody can have asked for it yet.
            self.ask(st, Instant::now(), Cue::Speak);
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
        settings: GameSettings,
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
            .save_game(name, summary.trim(), prompt.trim(), &settings, now_ms())
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

    /// The summary, settings and hidden prompt of a game, so the agent
    /// can revise it.
    pub fn read_game(&self, name: &str) -> Result<(String, GameSettings, String), String> {
        match self.store().game(name.trim()) {
            Ok(Some((g, prompt))) => Ok((g.summary, g.settings, prompt)),
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
        settings: GameSettings,
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
        match self.store().update_game(
            &old_name,
            &new_name,
            summary.trim(),
            prompt.trim(),
            &settings,
        ) {
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
            title: String::new(),
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
        self.reconsider(&mut st, now);
        self.follow_engagement(&mut st, now);
        let state = match &st.reply {
            None if self.will_answer(&st) => "waiting",
            None => "listening",
            Some(r) if r.sentences.iter().any(|s| s.played > 0) => "speaking",
            // Still only deciding whether to speak: the room shouldn't
            // flash "thinking" at every pause of a conversation that may
            // well not be with it.
            Some(r) if r.cue.deciding() && r.sentences.is_empty() => "listening",
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
            || st.people.values().any(|p| p.speaking || p.holding())
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
        // Being addressed by name starts a back-and-forth: from here on
        // the AI is given the turn at every pause, until the room goes
        // quiet. Someone speaking also cancels a silence it was waiting
        // on -- the silence didn't happen.
        let named = self.named(&said);
        st.reconsider = None;
        if st.mode == Mode::Named && named {
            self.engage(st, now);
        }
        match st.mode {
            // Not its name, so not its turn.
            Mode::Named => {}
            // Named, so it answers; otherwise it judges for itself
            // whether this pause is its moment.
            Mode::Auto if named => self.ask(st, now, Cue::Speak),
            Mode::Auto => self.ask(st, now, Cue::Decide { lull: None }),
            Mode::Always => self.ask(st, now, Cue::Speak),
        }
    }

    /// Give the AI the turn: to answer, or to decide whether to.
    fn ask(&self, st: &mut State, now: Instant, cue: Cue) {
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
            title: String::new(),
            text: String::new(),
            live: true,
            partial: false,
            cut: false,
            at: now_ms(),
            sent: true,
        };
        // While it is only deciding, its entry is held back: a turn it
        // passes on must leave nothing behind. follow_reply puts it on
        // screen as soon as there is something to show.
        if !cue.deciding() {
            self.emit_entry(&entry);
        }
        st.reply = Some(Reply {
            generation,
            entry: entry.id.clone(),
            cue,
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
            deciding = cue.deciding(),
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
                engaged: st.engaged.is_some(),
                cue,
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
        !st.people.values().any(|p| p.speaking || p.holding())
            && pending.peek().is_some()
            // In `Auto` it is the AI's call, so the room doesn't promise
            // an answer it may not give.
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
impl Person {
    /// Holding push to talk: not done talking, however long the pause.
    fn holding(&self) -> bool {
        self.ptt && !self.muted && self.mic
    }
}

fn paragraph_mode(ptt: bool) -> ParagraphMode {
    if ptt {
        ParagraphMode::Manual
    } else {
        ParagraphMode::Auto
    }
}

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
            Kind::Game | Kind::Card => true,
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
            Kind::Card => {
                // The card is written down where everyone can see it, so
                // the model is shown it as its own, and in full: this is
                // what the room is looking at.
                let note = format!(
                    "[You posted a card to the whole room, headed \"{}\":\n{}]",
                    e.title,
                    e.text.trim()
                );
                out.push((Role::Assistant, note));
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
            engaged_for: Duration::from_millis(5000),
            history: 50,
        };
        let r = Room::new(store, cfg, Arc::new(AtomicU64::new(0)), tx, None).unwrap();
        (r, rx)
    }

    /// A game that leaves the room's mode alone.
    fn lobby_game() -> GameSettings {
        GameSettings::default()
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
    fn the_roster_follows_the_invite_list() {
        let (r, _turns) = room(Mode::Named);
        let names = |r: &Room| {
            r.lock()
                .people
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(",")
        };
        assert_eq!(names(&r), "Alex,Ryan");

        // Invited while the room is running: appears on the next look.
        r.store().invite("Sam").unwrap();
        r.reconcile_people();
        assert_eq!(names(&r), "Alex,Ryan,Sam");

        // Uninvited while connected: stays, because they are still here.
        r.join("Sam");
        r.store().uninvite("Sam").unwrap();
        r.reconcile_people();
        assert_eq!(names(&r), "Alex,Ryan,Sam");

        // ...and goes once their last tab does.
        r.leave("Sam");
        r.reconcile_people();
        assert_eq!(names(&r), "Alex,Ryan");
    }

    #[test]
    fn push_to_talk_waits_for_release() {
        let (r, turns) = room(Mode::Always);
        r.set_mic("Ryan", true);
        r.set_ptt("Ryan", true);
        r.set_muted("Ryan", false);
        r.on_paragraph("Ryan", &para("p1", "Let me think about", false));
        r.tick(quiet(&r));
        assert!(
            turns.try_recv().is_err(),
            "a pause while holding isn't the end"
        );
        assert_eq!(r.lock().agent_state, "listening");
        r.on_paragraph(
            "Ryan",
            &para("p1", "Let me think about it. Okay, go.", false),
        );
        r.set_muted("Ryan", true);
        r.tick(quiet(&r));
        let t = turns.try_recv().expect("answered after release");
        assert!(t.messages.last().unwrap().1.contains("Okay, go."));
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
        r.save_game(g, "Dragon Heist", "A heist.", "Run a heist.", lobby_game())
            .unwrap();
        assert!(
            r.check_game_name("dragon heist").is_err(),
            "names are unique"
        );
        assert!(r
            .save_game(g, "DRAGON HEIST", "x", "y", lobby_game())
            .is_err());
        assert_eq!(cards(&r), ["Dragon Heist"]);

        // Revised (and renamed): the old card goes, a new one comes.
        assert_eq!(r.read_game("dragon heist").unwrap().2, "Run a heist.");
        let name = r
            .revise_game(
                g,
                "Dragon Heist",
                Some("Dragon Caper"),
                "A caper.",
                "Run a caper.",
                lobby_game(),
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
        r.revise_game(
            g,
            "Dragon Caper",
            None,
            "A caper.",
            "Run a better caper.",
            lobby_game(),
        )
        .unwrap();
        assert!(r.list_games()[0].played, "still marked played");
        assert_eq!(
            r.read_game("Dragon Caper").unwrap().2,
            "Run a better caper."
        );
        r.delete_game("Dragon Caper");
        assert!(r.list_games().is_empty());
    }

    #[test]
    fn a_game_started_by_voice_opens_after_the_reply_is_heard() {
        let (r, turns) = room(Mode::Named);
        let g0 = r.generation.load(Ordering::SeqCst);
        r.save_game(
            g0,
            "Quick Draw",
            "A showdown.",
            "Run a showdown.",
            lobby_game(),
        )
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
        r.save_game(g, "Zork", "Caves.", "Dark.", lobby_game())
            .unwrap();
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
        assert!(r
            .save_game(g, "Zork", "Caves.", "Dark.", lobby_game())
            .is_err());
        assert!(r.list_games().is_empty());
    }

    /// The room's effective mode, and whether a back-and-forth is on.
    fn mode_now(r: &Room) -> (Mode, bool) {
        let st = r.lock();
        (st.mode, st.engaged.is_some())
    }

    #[test]
    fn naming_it_starts_a_back_and_forth_that_lapses_when_quiet() {
        let (r, turns) = room(Mode::Named);
        // The room's clock, so the quiet can be waited out without
        // sleeping. reply_after is 1 s here and engaged_for 5 s.
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        assert_eq!(mode_now(&r), (Mode::Named, false));

        // Addressed by name: from here on it answers every pause.
        let mut events = r.subscribe();
        r.on_paragraph("Ryan", &para("p1", "Vox, what's the weather?", false));
        r.tick(at(1_100));
        assert!(turns.try_recv().is_ok(), "named");
        assert_eq!(mode_now(&r), (Mode::Auto, true));
        // What the tabs are told, which is what the buttons read.
        let mode_events: Vec<String> = std::iter::from_fn(|| events.try_recv().ok())
            .map(|e| e.json.to_string())
            .filter(|j| j.contains("\"type\":\"mode\""))
            .collect();
        assert_eq!(
            mode_events,
            [r#"{"engaged":true,"locked":false,"mode":"auto","type":"mode"}"#],
        );

        // The next thing said needs no name at all.
        r.stop_agent();
        r.on_paragraph("Ryan", &para("p2", "And tomorrow?", false));
        r.tick(at(2_200));
        assert!(turns.try_recv().is_ok(), "answered without being named");

        // While the AI is still answering, the quiet doesn't count.
        r.tick(at(20_000));
        assert_eq!(mode_now(&r), (Mode::Auto, true), "still replying");

        // Once it has stopped, a quiet room ends the back-and-forth...
        r.stop_agent();
        r.tick(at(22_000));
        assert_eq!(mode_now(&r), (Mode::Auto, true), "not quiet long enough");
        r.tick(at(26_000));
        assert_eq!(mode_now(&r), (Mode::Named, false));

        // ...and it is back to needing its name.
        r.on_paragraph("Ryan", &para("p3", "What do you think, Alex?", false));
        r.tick(at(27_200));
        assert!(turns.try_recv().is_err(), "not addressed");
    }

    #[test]
    fn the_ai_can_stand_down_by_itself() {
        let (r, turns) = room(Mode::Named);
        r.on_paragraph("Ryan", &para("p1", "Thanks Vox, that's all.", false));
        r.tick(quiet(&r));
        assert!(turns.try_recv().is_ok());
        assert_eq!(mode_now(&r), (Mode::Auto, true));

        assert_eq!(r.wait_until_named(), "named");
        assert_eq!(mode_now(&r), (Mode::Named, false));
        r.stop_agent();
        r.on_paragraph("Ryan", &para("p2", "So where were we, Alex?", false));
        r.tick(quiet(&r));
        assert!(turns.try_recv().is_err(), "stood down");
    }

    #[test]
    fn a_game_can_require_its_own_mode() {
        let (r, turns) = room(Mode::Named);
        let g = r.generation.load(Ordering::SeqCst);
        r.save_game(
            g,
            "Dragon Heist",
            "A heist.",
            "Run a heist.",
            GameSettings {
                reply: ReplyMode::Always,
                ..GameSettings::default()
            },
        )
        .unwrap();
        r.enter_game("Dragon Heist");
        // The game opens by itself, and answers every pause while it is
        // played -- without anyone having said the AI's name.
        assert!(turns.try_recv().is_ok(), "the game opens");
        assert_eq!(mode_now(&r), (Mode::Always, false));
        assert!(r.lock().game_mode.is_some(), "the buttons are locked");
        r.stop_agent();
        r.on_paragraph("Ryan", &para("p1", "I check the door.", false));
        r.tick(quiet(&r));
        assert!(turns.try_recv().is_ok(), "no name needed in this game");

        // A quiet room doesn't end what the game requires.
        r.stop_agent();
        r.tick(Instant::now() + r.cfg.engaged_for * 2);
        assert_eq!(mode_now(&r), (Mode::Always, false), "the game still rules");
        // Nor does the AI standing down.
        assert_eq!(r.wait_until_named(), "always");

        // Back in the lobby it is the people's mode again.
        r.lobby();
        assert_eq!(mode_now(&r), (Mode::Named, false));
        assert!(r.lock().game_mode.is_none());
    }

    #[test]
    fn how_many_players_a_game_is_for() {
        let p = |min, max| Players { min, max };
        assert_eq!(p(Some(4), Some(4)).describe(), "4");
        assert_eq!(p(Some(2), Some(5)).describe(), "2 to 5");
        assert_eq!(p(Some(3), None).describe(), "3 or more");
        assert_eq!(p(None, Some(6)).describe(), "up to 6");
        assert_eq!(p(None, None).describe(), "any number");

        assert!(p(Some(5), Some(2)).check().is_err(), "backwards");
        assert!(p(Some(0), None).check().is_err(), "nobody can play it");
        assert!(p(None, Some(500)).check().is_err(), "a typo, not a game");
        assert!(p(Some(2), Some(5)).check().is_ok());
        assert!(p(None, None).check().is_ok());
    }

    #[test]
    fn a_games_mode_is_part_of_what_is_saved() {
        let (r, _turns) = room(Mode::Named);
        let g = r.generation.load(Ordering::SeqCst);
        r.save_game(
            g,
            "Quick Draw",
            "A showdown.",
            "Run it.",
            GameSettings {
                reply: ReplyMode::Always,
                ..GameSettings::default()
            },
        )
        .unwrap();
        assert_eq!(r.list_games()[0].settings.reply, ReplyMode::Always);
        assert_eq!(
            r.read_game("quick draw").unwrap().1.reply,
            ReplyMode::Always
        );
        // Revising keeps whatever it is revised to.
        r.revise_game(
            g,
            "Quick Draw",
            None,
            "A showdown.",
            "Run it.",
            lobby_game(),
        )
        .unwrap();
        assert_eq!(r.list_games()[0].settings.reply, ReplyMode::Room);
    }

    /// Everything the tabs were told since `events` was taken.
    fn events_of(events: &mut broadcast::Receiver<Event>, kind: &str) -> Vec<String> {
        std::iter::from_fn(|| events.try_recv().ok())
            .map(|e| e.json.to_string())
            .filter(|j| j.contains(&format!("\"type\":\"{kind}\"")))
            .collect()
    }

    #[test]
    fn in_auto_mode_the_ai_is_given_the_turn_and_may_hand_it_straight_back() {
        let (r, turns) = room(Mode::Auto);
        let mut events = r.subscribe();
        r.on_paragraph(
            "Ryan",
            &para("p1", "So what did you make of it, Alex?", false),
        );
        r.tick(quiet(&r));

        // The room doesn't promise an answer it isn't sure of...
        assert_eq!(r.lock().agent_state, "listening");
        // ...but the AI does get the turn, to judge for itself.
        let t = turns.try_recv().expect("given the turn");
        assert_eq!(t.cue, Cue::Decide { lull: None });
        assert_eq!(r.lock().agent_state, "listening", "not even thinking");
        assert!(
            events_of(&mut events, "entry")
                .iter()
                .all(|e| !e.contains("\"agent\"")),
            "nothing of the AI's is on screen yet",
        );

        // It judges the pause to be the people's, and says so.
        r.stay_quiet(t.generation, None).unwrap();
        assert!(r.lock().reply.is_none());
        assert_eq!(r.lock().agent_state, "listening");
        // The conversation is exactly what the people said.
        assert_eq!(
            r.lock()
                .log
                .iter()
                .map(|e| (e.kind, e.text.clone()))
                .collect::<Vec<_>>(),
            [(Kind::Human, "So what did you make of it, Alex?".to_string())],
            "the turn it passed on left nothing behind",
        );
    }

    #[test]
    fn it_can_ask_to_be_shown_a_longer_silence() {
        let (r, turns) = room(Mode::Auto);
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        r.on_paragraph(
            "Ryan",
            &para("p1", "I don't know what that was about.", false),
        );
        r.tick(at(1_100));
        let t = turns.try_recv().expect("given the turn");

        // Not now -- but show me the room again if the quiet lasts.
        r.stay_quiet(t.generation, Some(3)).unwrap();
        r.tick(at(2_000));
        assert!(turns.try_recv().is_err(), "not yet");
        r.tick(at(5_000));
        let again = turns.try_recv().expect("shown the silence it asked for");
        assert_eq!(again.cue, Cue::Decide { lull: Some(3) });

        // A second pass is the end of it: the room doesn't go round again
        // while nobody is saying anything.
        r.stay_quiet(again.generation, Some(3)).unwrap();
        r.tick(at(30_000));
        assert!(turns.try_recv().is_err(), "once per silence");
        assert!(r.lock().reconsider.is_none());
    }

    #[test]
    fn someone_speaking_cancels_the_silence_it_was_waiting_on() {
        let (r, turns) = room(Mode::Auto);
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        r.on_paragraph("Ryan", &para("p1", "Anyway.", false));
        r.tick(at(1_100));
        let t = turns.try_recv().expect("given the turn");
        r.stay_quiet(t.generation, Some(5)).unwrap();

        // The silence didn't happen: someone carried on talking, so the
        // next turn comes from what they said, not from the old wait.
        r.on_paragraph("Alex", &para("p2", "Where were we?", false));
        r.tick(at(2_300));
        assert!(r.lock().reconsider.is_none());
        let next = turns.try_recv().expect("the new pause");
        assert_eq!(next.cue, Cue::Decide { lull: None });
        assert!(next.messages[0].1.contains("Where were we?"));
    }

    #[test]
    fn staying_quiet_is_only_for_a_turn_it_was_asked_to_judge() {
        let (r, turns) = room(Mode::Always);
        r.on_paragraph("Ryan", &para("p1", "Go on then.", false));
        r.tick(quiet(&r));
        let t = turns.try_recv().expect("answers every pause");
        assert_eq!(t.cue, Cue::Speak);
        assert!(
            r.stay_quiet(t.generation, None).is_err(),
            "it was told to answer, not asked whether to"
        );
        assert!(r.lock().reply.is_some(), "the reply stands");
        // And a turn the room has moved past is nobody's to pass on.
        assert!(r.stay_quiet(t.generation + 99, None).is_err());
    }

    /// The cards one person is holding.
    fn held(r: &Room, person: &str) -> Vec<String> {
        let st = r.lock();
        r.cards_for(&st, person)
            .into_iter()
            .map(|c| format!("{}: {}", c.title, c.body))
            .collect()
    }

    #[test]
    fn a_card_is_sent_to_one_person_and_to_nobody_else() {
        let (r, _turns) = room(Mode::Named);
        let mut events = r.subscribe();
        assert_eq!(
            r.send_card("ryan", " Your role ", " You are the traitor. "),
            Ok("Ryan".into()),
            "the name is matched ignoring case"
        );

        // The one event it made is addressed to Ryan, so only Ryan's tabs
        // are written to -- Alex's socket never sees the body at all.
        let ev = events.try_recv().expect("one event");
        assert!(ev.is_for("Ryan") && !ev.is_for("Alex"));
        assert!(ev.json.contains("You are the traitor."));
        assert!(events.try_recv().is_err(), "nothing was broadcast");

        assert_eq!(held(&r, "Ryan"), ["Your role: You are the traitor."]);
        assert!(held(&r, "Alex").is_empty());

        // The cards are the AI's to give and to take back; the person
        // holding one has no way to clear it.
        assert!(r.take_back_card("Ryan", "no such card").is_err());
        assert_eq!(
            r.take_back_card("ryan", " your role "),
            Ok("Your role".into())
        );
        assert!(held(&r, "Ryan").is_empty());
    }

    #[test]
    fn a_card_needs_somebody_to_send_it_to() {
        let (r, _turns) = room(Mode::Named);
        let err = r.send_card("Morgan", "Your role", "You are the traitor.");
        assert!(
            err.as_ref().is_err_and(|e| e.contains("Alex, Ryan")),
            "the error names who is here: {err:?}"
        );
        assert!(r.send_card("Ryan", "", "nothing titled").is_err());
        assert!(r.send_card("Ryan", "Empty", "   ").is_err());
        assert!(r
            .send_card("Ryan", &"x".repeat(CARD_TITLE_MAX + 1), "body")
            .is_err());
        assert!(held(&r, "Ryan").is_empty());
    }

    #[test]
    fn the_ai_decides_what_everyone_is_holding() {
        let (r, _turns) = room(Mode::Named);
        let mut events = r.subscribe();
        r.send_card("Ryan", "Your hand", "Two aces.").unwrap();
        let first = r.cards_for(&r.lock(), "Ryan")[0].id.clone();

        // The same title again rewrites that card rather than stacking
        // another beside it, so a hand can change as it is played.
        r.send_card("Ryan", "your hand", "Two aces and the queen.")
            .unwrap();
        assert_eq!(held(&r, "Ryan"), ["your hand: Two aces and the queen."]);
        assert_eq!(
            r.cards_for(&r.lock(), "Ryan")[0].id,
            first,
            "the same card, so it stays where it is on screen"
        );
        let card_events = events_of(&mut events, "card");
        assert_eq!(card_events.len(), 2);
        assert!(card_events[0].contains(r#""replaced":false"#));
        assert!(card_events[1].contains(r#""replaced":true"#));

        // And the AI can take it back, which is the only way one goes.
        assert_eq!(
            r.take_back_card("Ryan", "Your Hand"),
            Ok("your hand".into())
        );
        assert!(held(&r, "Ryan").is_empty());
        assert!(events_of(&mut events, "card_removed")[0].contains(&first));
        let err = r.take_back_card("Ryan", "Your hand").unwrap_err();
        assert!(err.contains("none at all"), "{err}");
    }

    #[test]
    fn a_posted_card_is_in_the_conversation_for_good() {
        let (r, _turns) = room(Mode::Named);
        let mut events = r.subscribe();
        assert!(r.post_card("  ", "nothing titled").is_err());
        assert!(r.post_card("The rules", "   ").is_err());
        assert_eq!(
            r.post_card(" The rules ", " One card each.\nAces are low. "),
            Ok("The rules".into())
        );

        // It is in the conversation, for everyone -- not addressed to
        // anyone, the way a dealt card is.
        let posted = events_of(&mut events, "entry");
        assert_eq!(posted.len(), 1);
        assert!(posted[0].contains(r#""kind":"card""#), "{}", posted[0]);
        assert!(posted[0].contains(r#""title":"The rules""#));
        assert!(posted[0].contains("Aces are low."));

        let entry = r.lock().log.last().cloned().unwrap();
        assert_eq!(entry.kind, Kind::Card);
        assert_eq!((entry.title.as_str(), entry.live), ("The rules", false));

        // And the AI is shown its own card in full, so it can go on
        // playing by what the room is looking at.
        let said = conversation_messages(&r.lock().log, 50);
        assert_eq!(said.len(), 1);
        assert_eq!(said[0].0, Role::Assistant);
        assert!(said[0].1.contains("The rules"), "{}", said[0].1);
        assert!(said[0].1.contains("Aces are low."));
    }

    #[test]
    fn the_ai_can_read_back_what_it_has_dealt() {
        let (r, _turns) = room(Mode::Named);
        r.send_card("Ryan", "Your role", "You are the traitor.")
            .unwrap();
        r.send_card("Alex", "Your hand", "Two aces.").unwrap();

        let all = r.read_cards(None);
        assert_eq!(all.len(), 2, "everyone, holding or not");
        assert_eq!(all[1]["name"], "Ryan");
        assert_eq!(all[1]["cards"][0]["body"], "You are the traitor.");

        let one = r.read_cards(Some("alex"));
        assert_eq!(one.len(), 1);
        assert_eq!(one[0]["cards"][0]["title"], "Your hand");
        assert!(
            r.read_cards(Some("Morgan")).is_empty(),
            "nobody by that name"
        );
    }

    #[test]
    fn cards_belong_to_the_conversation_that_dealt_them() {
        let (r, _turns) = room(Mode::Named);
        r.send_card("Ryan", "Your role", "You are the traitor.")
            .unwrap();
        r.send_card("Alex", "Your hand", "Two aces.").unwrap();
        // What the AI is shown: who is here, and the title of each card
        // it has given them, which is how it addresses one again.
        assert_eq!(
            r.people_here()
                .iter()
                .map(|p| format!("{}: {}", p["name"], p["cards"]))
                .collect::<Vec<_>>(),
            [r#""Alex": ["Your hand"]"#, r#""Ryan": ["Your role"]"#],
        );
        r.new_conversation();
        assert!(
            held(&r, "Ryan").is_empty(),
            "a fresh conversation deals nothing"
        );
        assert!(r.hello("Ryan")["cards"].as_array().unwrap().is_empty());
        // The old conversation's cards are still in the database, for it.
        r.send_card("Ryan", "Again", "A new secret.").unwrap();
        assert_eq!(held(&r, "Ryan"), ["Again: A new secret."]);
    }
}
