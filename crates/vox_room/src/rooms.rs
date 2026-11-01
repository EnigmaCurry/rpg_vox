//! The rooms a server holds, and who is in which.
//!
//! One `vox_room` is several rooms. Everybody calls in once, on one
//! socket, and the room they are in is a thing they change on it: the
//! audio connection never drops, and what moves is where their voice
//! goes and whose conversation they are shown. See [`crate::web`] for
//! the socket, and ROOM.md for what it looks like.
//!
//! There are two kinds. The ones named with `--room` are **shared**: the
//! house rooms, there whether anybody is in them or not, because
//! somebody has to be able to walk into one and wait. And everybody has
//! one **of their own** -- `@NAME`, admitting the one person whose
//! invite reads that name -- which is made the first time they ask for
//! it and then stays. A room of your own is where you can talk to the
//! AI without being overheard: its roster is you, its conversation is
//! yours, and nobody else can ask for it.
//!
//! Each room is a whole [`Room`]: its own conversation, its own mixer,
//! its own voices and its own agent. What they share is the expensive
//! and unchanging part -- the loaded speech-to-text models, the
//! translator, the endpoint the AI is asked on -- and this one database
//! file, where each room's settings are kept under its own name.
//!
//! The clocks are here rather than per room, because a thread per room
//! waking fifty times a second to find nobody in it is a poor way to
//! hold an empty room open.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context as _, Result};
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tracing::{error, info};

use crate::room::{Mode, Room, RoomConfig};
use crate::store::Invited;

/// How often the rooms are told the time: turn taking, the meters, and
/// now and then the invite list. The same 50 ms one room used to run on.
const TICK: Duration = Duration::from_millis(50);
/// The mixer's clock: one 20 ms frame of audio for every listener in
/// every room.
const FRAME: Duration = Duration::from_millis(20);
/// Ticks between looks at the invite list, as it always was: two a
/// second is often enough to notice `vox_room invite` in another
/// process, and not so often that an idle server is reading the
/// database for nothing.
const RECONCILE_EVERY: u32 = 40;

/// Builds one room, with everything in it: its store, its mixer, its
/// voices, its interpreters and the agent that answers there.
pub type Make = Box<dyn Fn(RoomConfig) -> Result<Arc<Room>> + Send + Sync>;

/// How long a room's name may be. It goes in a database key and on a
/// button; mostly this is here so a typo in `--room` is a complaint at
/// startup rather than a button nobody can read.
const NAME_MAX: usize = 32;

pub struct Rooms {
    /// The shared rooms, in the order they were named. The first is
    /// where a tab lands.
    shared: Vec<Arc<Room>>,
    /// Somebody's own room, by its id, made the first time they ask.
    own: Mutex<BTreeMap<String, Arc<Room>>>,
    make: Make,
    /// The AI's name, which is half the title of everyone's own room.
    agent_name: String,
    /// Somebody came in, left, or changed rooms. The directory every
    /// tab is shown is worked out per person -- a room of your own is
    /// nobody else's business -- so what goes out is only that it has
    /// changed, and each socket asks again for its own.
    news: broadcast::Sender<()>,
}

/// A name for a room that can be a database key and a button.
fn check_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        bail!("a room needs a name");
    }
    if name.chars().count() > NAME_MAX {
        bail!("a room's name is at most {NAME_MAX} characters, so {name:?} is too long");
    }
    // `@` opens somebody's own room and `:` separates a room from its
    // settings in the database, so neither is a shared room's to use.
    if let Some(c) = name.chars().find(|c| *c == '@' || *c == ':' || c.is_control()) {
        bail!("a room's name can't contain {c:?}, so {name:?} won't do");
    }
    Ok(name.to_string())
}

impl Rooms {
    /// The shared rooms `names` asks for, built now. Everybody's own
    /// room is not: an empty room of one costs nothing while it is only
    /// a button, and most people's will never be opened.
    pub fn new(base: &RoomConfig, names: &[String], make: Make) -> Result<Arc<Self>> {
        let mut shared = Vec::new();
        for name in names {
            let name = check_name(name)?;
            if shared
                .iter()
                .any(|r: &Arc<Room>| r.config().id.eq_ignore_ascii_case(&name))
            {
                bail!("two rooms called {name:?}");
            }
            let cfg = RoomConfig {
                id: name.clone(),
                title: name.clone(),
                only: None,
                // The house room waits to be named, so two people can
                // talk in it without the AI butting in.
                default_mode: Mode::Named,
                ..base.clone()
            };
            shared.push(make(cfg).with_context(|| format!("open the {name} room"))?);
        }
        if shared.is_empty() {
            bail!("no rooms: --room NAME, at least once");
        }
        info!(
            rooms = %names.join(", "),
            "the rooms of the house (and one of their own for everybody)"
        );
        let (news, _) = broadcast::channel(16);
        Ok(Arc::new(Self {
            shared,
            own: Mutex::new(BTreeMap::new()),
            make,
            agent_name: base.agent_name.clone(),
            news,
        }))
    }

    /// The id of the room belonging to the person whose invite reads
    /// `name`. The invite's name and not the one they call themselves:
    /// a room is theirs for as long as they are invited, and renaming
    /// yourself at the door must not leave your conversation behind.
    pub fn own_id(name: &str) -> String {
        format!("@{name}")
    }

    fn own_title(&self) -> String {
        format!("You and {}", self.agent_name)
    }

    /// The room a tab lands in: the first one named.
    pub fn home(&self) -> Arc<Room> {
        self.shared[0].clone()
    }

    /// The room called `id`, if `who` may be in it -- which is every
    /// shared room, and the one of their own. Somebody's own room is
    /// built here, the first time they ask for it. None is a room that
    /// isn't there, or is somebody else's.
    pub fn find(&self, who: &Invited, id: &str) -> Option<Arc<Room>> {
        if let Some(r) = self.shared.iter().find(|r| r.config().id == id) {
            return Some(r.clone());
        }
        if id != Self::own_id(&who.name) {
            return None;
        }
        // Under the lock while it is being built, so two tabs of theirs
        // asking at once get the one room rather than two.
        let mut own = self.own.lock().expect("rooms mutex poisoned");
        if let Some(r) = own.get(id) {
            return Some(r.clone());
        }
        let cfg = RoomConfig {
            id: id.to_string(),
            title: self.own_title(),
            only: Some(who.name.clone()),
            // Nobody else is in here to take the turn from, and having
            // to say its name to a room with only the two of you in it
            // is a ceremony with no point to it.
            default_mode: Mode::Always,
            ..self.shared[0].config().clone()
        };
        match (self.make)(cfg) {
            Ok(room) => {
                info!(person = %who.name, room = id, "opened a room of their own");
                own.insert(id.to_string(), room.clone());
                Some(room)
            }
            Err(err) => {
                error!(person = %who.name, "couldn't open their own room: {err:#}");
                None
            }
        }
    }

    /// The rooms `who` may go to, with who is in each: the house, then
    /// theirs. Their own room is listed whether or not it has been
    /// built -- the list is what they press to build it.
    pub fn directory(&self, who: &Invited) -> Value {
        let mut rooms: Vec<Value> = self
            .shared
            .iter()
            .map(|r| {
                json!({
                    "id": r.config().id,
                    "title": r.config().title,
                    "private": false,
                    "here": r.here(),
                })
            })
            .collect();
        let id = Self::own_id(&who.name);
        let here = self
            .own
            .lock()
            .expect("rooms mutex poisoned")
            .get(&id)
            .map(|r| r.here())
            .unwrap_or_default();
        rooms.push(json!({
            "id": id,
            "title": self.own_title(),
            "private": true,
            "here": here,
        }));
        json!({ "type": "rooms", "rooms": rooms })
    }

    /// Told when who-is-where changes, so every tab can ask for the
    /// directory again.
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.news.subscribe()
    }

    pub fn announce(&self) {
        let _ = self.news.send(());
    }

    /// Have every room read the invite list again. For when something
    /// that is the person's and not the room's has been written to the
    /// database -- the name they call themselves, the language they
    /// hear -- so the rooms they are not in catch up with it too,
    /// rather than at the next look a few ticks from now.
    pub fn catch_up(&self) {
        for room in self.all() {
            room.reconcile_people();
        }
    }

    /// Every room that exists right now.
    fn all(&self) -> Vec<Arc<Room>> {
        let mut all = self.shared.clone();
        all.extend(self.own.lock().expect("rooms mutex poisoned").values().cloned());
        all
    }

    /// Start the two clocks every room runs on: the mixer's 20 ms frame
    /// and the 50 ms tick that takes the turns and moves the meters.
    pub fn spawn_clocks(self: &Arc<Self>) -> Result<()> {
        let me = self.clone();
        std::thread::Builder::new()
            .name("vox-room-mixer".into())
            .spawn(move || clock(FRAME, "mixer", move || {
                for room in me.all() {
                    room.mixer.tick();
                }
            }))
            .context("spawn mixer thread")?;
        let me = self.clone();
        std::thread::Builder::new()
            .name("vox-room-tick".into())
            .spawn(move || {
                let mut ticks: u32 = 0;
                clock(TICK, "tick", move || {
                    ticks = ticks.wrapping_add(1);
                    let now = Instant::now();
                    for room in me.all() {
                        room.tick(now);
                        // Meters at 10 per second.
                        if ticks % 2 == 0 {
                            room.emit_levels();
                        }
                        // `vox_room invite` and `uninvite` write to the
                        // database from their own process, so look for
                        // their work now and then rather than only at
                        // startup.
                        if ticks.is_multiple_of(RECONCILE_EVERY) {
                            room.reconcile_people();
                        }
                    }
                })
            })
            .context("spawn tick thread")?;
        Ok(())
    }
}

/// Call `step` every `every`, without bursting to catch up after a
/// suspended laptop. The mixer's own loop, lifted out so both clocks
/// drift the same way.
fn clock(every: Duration, what: &str, mut step: impl FnMut()) {
    let mut next = Instant::now();
    loop {
        step();
        next += every;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else if now - next > every * 5 {
            tracing::warn!(
                behind_ms = (now - next).as_millis() as u64,
                "{what} clock reset"
            );
            next = now;
        }
    }
}
