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
use crate::store::{Invited, Store};

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

/// The address of the room of your own, whoever you are.
pub const OWN_PATH: &str = "/";
/// The address of the first room named with `--room`, where a tab that
/// says nothing about where it wants to be lands.
pub const LOBBY_PATH: &str = "/lobby";

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
    /// The invite list, the roles and the grants, which belong to the
    /// house rather than to any room in it. Its own handle on the same
    /// database file: who may go where is asked on the way into a room,
    /// and asking a room whether somebody may enter it is the wrong way
    /// round.
    people: Mutex<Store>,
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
    if let Some(c) = name
        .chars()
        .find(|c| *c == '@' || *c == ':' || c.is_control())
    {
        bail!("a room's name can't contain {c:?}, so {name:?} won't do");
    }
    Ok(name.to_string())
}

impl Rooms {
    /// The shared rooms `names` asks for, built now. Everybody's own
    /// room is not: an empty room of one costs nothing while it is only
    /// a button, and most people's will never be opened.
    pub fn new(
        people: Store,
        base: &RoomConfig,
        names: &[String],
        make: Make,
    ) -> Result<Arc<Self>> {
        // Every name settled before anything is built, so a server
        // started with a bad list says so instead of opening half the
        // house and then failing.
        let mut wanted: Vec<String> = Vec::new();
        for name in names {
            let name = check_name(name)?;
            if wanted.iter().any(|w| w.eq_ignore_ascii_case(&name)) {
                bail!("two rooms called {name:?}");
            }
            wanted.push(name);
        }
        if wanted.is_empty() {
            bail!("no rooms: --room NAME, at least once");
        }
        let mut shared = Vec::new();
        for name in &wanted {
            let cfg = RoomConfig {
                id: name.clone(),
                title: name.clone(),
                only: None,
                // The house room waits to be named, so two people can
                // talk in it without the AI butting in.
                default_mode: Mode::Named,
                ..base.clone()
            };
            let room = make(cfg).with_context(|| format!("open the {name} room"))?;
            shared.push(room);
        }
        info!(
            rooms = %names.join(", "),
            "the rooms of the house (and one of their own for everybody)"
        );
        // Everybody invited keeps every room they could already walk
        // into. Rooms are given out from here on, but giving them out
        // must not take one away from somebody who had it yesterday.
        // Everything but the lobby: that one is nobody's to be given,
        // and a row saying somebody may go where nobody can be refused
        // is a row that can only ever be wrong.
        match people.grant_everyone(&wanted[1..]) {
            Ok(true) => info!("everybody invited was given every room they could already go to"),
            Ok(false) => {}
            Err(err) => bail!("hand out the rooms people already had: {err:#}"),
        }
        let (news, _) = broadcast::channel(16);
        let rooms = Arc::new(Self {
            shared,
            own: Mutex::new(BTreeMap::new()),
            make,
            people: Mutex::new(people),
            agent_name: base.bot().name.clone(),
            news,
        });
        for room in &rooms.shared {
            rooms.watch(room);
        }
        Ok(rooms)
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

    /// Whether `who` may go to the shared room called `id`.
    ///
    /// Everybody has the lobby, because a room nobody can be refused is
    /// what calling in is for, and the owner has the house. Every other
    /// room is given out one person at a time. A room of your own is
    /// not asked about here: it is yours because it is yours, and the
    /// owner has no key to it either -- "just you and Vox" is a promise
    /// the room makes on its face, and an owner who could walk into it
    /// would make that a lie.
    fn may_enter(&self, who: &Invited, id: &str) -> bool {
        if who.owner || id == self.shared[0].config().id {
            return true;
        }
        match self.people().granted(&who.name, id) {
            Ok(ok) => ok,
            Err(err) => {
                // A database that won't answer is not a key: nobody is
                // let into a room on the strength of a failed question.
                error!(person = %who.name, room = id, "whether they may come in: {err:#}");
                false
            }
        }
    }

    fn people(&self) -> std::sync::MutexGuard<'_, Store> {
        self.people.lock().expect("people mutex poisoned")
    }

    // -- the house's people, for the owner --------------------------------
    //
    // The invite list belongs to the house and not to any room in it, so
    // this is where it is asked about and changed. Who is allowed to ask
    // is [`crate::web`]'s business: these do what they are told.

    /// Everybody invited, in name order.
    pub fn everyone(&self) -> Result<Vec<Invited>> {
        self.people().people()
    }

    /// Invite somebody, or hand back the link they have not used yet.
    pub fn invite(&self, name: &str) -> Result<String> {
        self.people().invite(name)
    }

    /// A fresh link for somebody already invited: the way back in after
    /// a link goes astray. Their rooms, their name and the browsers
    /// they are already in stay as they are -- cutting those is what
    /// removing them is for.
    pub fn relink(&self, name: &str) -> Result<String> {
        let people = self.people();
        if people.person(name)?.is_none() {
            bail!("{name} isn't invited");
        }
        people.relink(name)
    }

    /// Revoke somebody: their link, their browsers and their rooms.
    pub fn uninvite(&self, name: &str) -> Result<bool> {
        self.people().uninvite(name)
    }

    /// Give somebody a room, or take it away.
    pub fn set_grant(&self, name: &str, room: &str, allowed: bool) -> Result<bool> {
        if !self.shared.iter().any(|r| r.config().id == room) {
            bail!("there is no room called {room:?}");
        }
        if room == self.shared[0].config().id {
            bail!("the lobby is everybody's; it isn't given out");
        }
        self.people().set_grant(name, room, allowed)
    }

    /// The rooms somebody has been given, the lobby and their own aside.
    pub fn grants_of(&self, name: &str) -> Result<Vec<String>> {
        self.people().grants(name)
    }

    /// The shared rooms, by id and title, in the order they were named.
    /// The first is the lobby, which everybody has.
    pub fn shared_rooms(&self) -> Vec<(String, String)> {
        self.shared
            .iter()
            .map(|r| (r.config().id.clone(), r.config().title.clone()))
            .collect()
    }

    /// The room called `id`, if `who` may be in it: the lobby, a room
    /// they have been given, or the one of their own. Somebody's own
    /// room is built here, the first time they ask for it. None is a
    /// room that isn't there, is somebody else's, or is one they
    /// haven't been given -- the three are one answer on purpose, so a
    /// door nobody may open says no more than that it won't.
    pub fn find(self: &Arc<Self>, who: &Invited, id: &str) -> Option<Arc<Room>> {
        if let Some(r) = self.shared.iter().find(|r| r.config().id == id) {
            return self.may_enter(who, id).then(|| r.clone());
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
                self.watch(&room);
                own.insert(id.to_string(), room.clone());
                Some(room)
            }
            Err(err) => {
                error!(person = %who.name, "couldn't open their own room: {err:#}");
                None
            }
        }
    }

    /// The rooms `who` may go to, with who is in each: theirs first,
    /// then the house's in the order they were named. Their own room is
    /// listed whether or not it has been built -- the list is what they
    /// press to build it.
    ///
    /// Theirs is first and stays first, wherever they are and however
    /// many rooms the house grows: the one room that is always there
    /// and always the same is the one to put where the eye lands, and a
    /// room that moved about the list as people came and went would be
    /// a room you had to look for.
    ///
    /// Each carries the address it lives at, which is what the browser
    /// puts in the bar and what somebody pastes to somebody else. The
    /// scheme is settled here rather than in the browser so there is
    /// one place that knows it:
    ///
    /// * `/` -- the room of your own. It is per person, and so is the
    ///   address: whoever opens it gets theirs.
    /// * `/lobby` -- the first room named with `--room`, which is where
    ///   a tab lands.
    /// * `/room/<link>` -- every other room, by the unguessable id it
    ///   was given when it was first opened. Not by name: a name is a
    ///   thing people change their minds about, and a link that changed
    ///   with it is a link somebody has already sent on.
    pub fn directory(&self, who: &Invited) -> Value {
        let id = Self::own_id(&who.name);
        let here = self
            .own
            .lock()
            .expect("rooms mutex poisoned")
            .get(&id)
            .map(|r| r.here())
            .unwrap_or_default();
        let mut rooms = vec![json!({
            "id": id,
            "title": self.own_title(),
            "path": OWN_PATH,
            "private": true,
            "here": here,
            "game": Value::Null,
        })];
        rooms.extend(
            self.shared
                .iter()
                .enumerate()
                .filter(|(_, r)| {
                    // A room they haven't been given is not on the list. A
                    // greyed-out room is a room to ask about, and the answer
                    // would always be the same.
                    self.may_enter(who, &r.config().id)
                })
                .map(|(n, r)| {
                    json!({
                        "id": r.config().id,
                        "title": r.config().title,
                        "path": self.path_of(n, r),
                        "private": false,
                        "here": r.here(),
                        // A room with a game on is doing something, and the
                        // people not in it may as well be told what.
                        "game": r.game_name(),
                    })
                }),
        );
        // Whether they own the house goes with the list, because it is
        // the same kind of thing: what this person may do, worked out
        // for them and sent to their tabs alone.
        json!({ "type": "rooms", "rooms": rooms, "owner": who.owner })
    }

    /// Where the `n`th shared room lives. The first is the lobby, and
    /// gets the short address people can say out loud.
    fn path_of(&self, n: usize, room: &Arc<Room>) -> String {
        if n == 0 {
            LOBBY_PATH.to_string()
        } else {
            format!("/room/{}", room.link())
        }
    }

    /// Have `room` tell us when the list would show it differently --
    /// it starting or finishing a game. Only a nudge goes out: the
    /// list itself is worked out per person, afterwards, on the socket
    /// that is going to send it.
    fn watch(self: &Arc<Self>, room: &Arc<Room>) {
        let me = Arc::downgrade(self);
        room.on_change(Box::new(move || {
            if let Some(me) = me.upgrade() {
                me.announce();
            }
        }));
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
        all.extend(
            self.own
                .lock()
                .expect("rooms mutex poisoned")
                .values()
                .cloned(),
        );
        all
    }

    /// Start the two clocks every room runs on: the mixer's 20 ms frame
    /// and the 50 ms tick that takes the turns and moves the meters.
    pub fn spawn_clocks(self: &Arc<Self>) -> Result<()> {
        let me = self.clone();
        std::thread::Builder::new()
            .name("vox-room-mixer".into())
            .spawn(move || {
                clock(FRAME, "mixer", move || {
                    for room in me.all() {
                        room.mixer.tick();
                    }
                })
            })
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
                        if ticks.is_multiple_of(2) {
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::room::BotConfig;
    use crate::store::Store;

    fn base() -> RoomConfig {
        RoomConfig {
            id: String::new(),
            title: String::new(),
            only: None,
            default_mode: Mode::Named,
            bots: vec![BotConfig::new("Vox", vec![])],
            reply_after: Duration::from_millis(1200),
            confirm: Duration::from_millis(300),
            engaged_for: Duration::from_secs(30),
            history: 60,
            language: "English".into(),
            languages: vec!["English".into()],
            spoken: vec!["English".into()],
        }
    }

    /// Rooms made out of nothing: a database of their own, no agent, no
    /// voices. Everything here is about which room you get, not what
    /// happens inside one.
    fn rooms(names: &[&str]) -> Arc<Rooms> {
        rooms_for(names, |_| {})
    }

    /// Rooms, with a chance to say who is invited and what they have
    /// been given before anything is built. The people store is its own
    /// database here, as it is its own handle in the real thing.
    fn rooms_for(names: &[&str], setup: impl Fn(&Store)) -> Arc<Rooms> {
        let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        let people = Store::memory();
        people.invite("Ryan").unwrap();
        people.invite("Alex").unwrap();
        setup(&people);
        Rooms::new(
            people,
            &base(),
            &names,
            Box::new(|cfg| {
                let store = Store::memory();
                store.invite("Ryan").unwrap();
                store.invite("Alex").unwrap();
                Room::new(store, cfg, None, BTreeMap::new(), None).map(|(room, wiring)| {
                    // The receivers would otherwise be dropped, and a
                    // turn sent to a closed channel is an error the
                    // room logs.
                    std::mem::forget(wiring);
                    room
                })
            }),
        )
        .unwrap()
    }

    /// Two rooms, with both people given the second: the ordering and
    /// the roster are what those tests are about, not who may come in.
    fn both_given_the_studio() -> Arc<Rooms> {
        rooms_for(&["Lounge", "Studio"], |p| {
            p.set_grant("Ryan", "Studio", true).unwrap();
            p.set_grant("Alex", "Studio", true).unwrap();
        })
    }

    fn who(name: &str) -> Invited {
        Invited {
            name: name.into(),
            display: name.into(),
            link: None,
            language: String::new(),
            owner: false,
        }
    }

    fn owner(name: &str) -> Invited {
        Invited {
            owner: true,
            ..who(name)
        }
    }

    #[test]
    fn a_room_of_your_own_is_yours_and_nobodys_else() {
        let rooms = both_given_the_studio();
        let ryan = who("Ryan");
        let alex = who("Alex");

        assert_eq!(rooms.home().config().id, "Lounge", "the first one named");
        assert!(rooms.find(&ryan, "Studio").is_some(), "the house's rooms");
        assert!(rooms.find(&alex, "Studio").is_some());

        // His is made the moment he asks for it, and is the same room
        // the second time.
        let mine = rooms.find(&ryan, "@Ryan").expect("his own");
        assert_eq!(mine.config().title, "You and Vox");
        assert_eq!(mine.config().only.as_deref(), Some("Ryan"));
        assert!(Arc::ptr_eq(&mine, &rooms.find(&ryan, "@Ryan").unwrap()));

        // And it is not Alex's to ask for, nor is a room nobody named.
        assert!(rooms.find(&alex, "@Ryan").is_none());
        assert!(rooms.find(&ryan, "Cellar").is_none());
        assert!(rooms.find(&ryan, "@Nobody").is_none());
    }

    #[test]
    fn the_directory_shows_the_house_and_your_own_room_only() {
        let rooms = both_given_the_studio();
        let ryan = who("Ryan");
        let alex = who("Alex");
        let ids = |d: &Value| {
            d["rooms"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["id"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        // Theirs first, then the house in the order it was named.
        assert_eq!(ids(&rooms.directory(&ryan)), ["@Ryan", "Lounge", "Studio"]);
        assert_eq!(ids(&rooms.directory(&alex)), ["@Alex", "Lounge", "Studio"]);

        // Listed before it exists -- the list is what you press to make
        // it -- and nobody is in it until somebody is.
        let own = |d: &Value| d["rooms"][0].clone();
        assert_eq!(own(&rooms.directory(&ryan))["here"], json!([]));
        assert_eq!(own(&rooms.directory(&ryan))["private"], json!(true));

        // Who is where follows the tabs, and the order doesn't move
        // with them: a room that wandered about the list would be one
        // you had to look for.
        rooms.find(&ryan, "Lounge").unwrap().join("Ryan", 1);
        rooms.find(&alex, "Studio").unwrap().join("Alex", 2);
        let d = rooms.directory(&ryan);
        assert_eq!(ids(&d), ["@Ryan", "Lounge", "Studio"]);
        let here = |d: &Value, i: usize| d["rooms"][i]["here"].clone();
        assert_eq!(here(&d, 1), json!(["Ryan"]));
        assert_eq!(here(&d, 2), json!(["Alex"]));
        // Even though Alex is on the Studio's roster, Ryan's own room
        // doesn't have her on its.
        assert_eq!(
            rooms.find(&ryan, "@Ryan").unwrap().config().only.as_deref(),
            Some("Ryan")
        );
    }

    /// Everybody has the lobby and the room of their own. Every other
    /// room is one person at a time, and until it is given the room is
    /// not on their list and not behind its address either -- a room
    /// you may not go to is a room that isn't there, as far as the
    /// server will say.
    #[test]
    fn a_room_is_given_out_one_person_at_a_time() {
        let rooms = rooms(&["Lounge", "Studio"]);
        let ryan = who("Ryan");
        let ids = |who: &Invited| {
            rooms.directory(who)["rooms"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["id"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };

        assert!(
            rooms.find(&ryan, "Lounge").is_some(),
            "the lobby is everybody's"
        );
        assert!(rooms.find(&ryan, "@Ryan").is_some(), "and so is his own");
        assert!(rooms.find(&ryan, "Studio").is_none(), "the Studio is not");
        assert_eq!(ids(&ryan), ["@Ryan", "Lounge"], "nor is it on his list");

        rooms.set_grant("Ryan", "Studio", true).unwrap();
        assert!(rooms.find(&ryan, "Studio").is_some(), "given");
        assert_eq!(ids(&ryan), ["@Ryan", "Lounge", "Studio"]);
        assert_eq!(rooms.grants_of("Ryan").unwrap(), ["Studio"]);

        // Taken away again, and he is out of it.
        rooms.set_grant("Ryan", "Studio", false).unwrap();
        assert!(rooms.find(&ryan, "Studio").is_none());
        assert_eq!(ids(&ryan), ["@Ryan", "Lounge"]);

        // The lobby is nobody's to give or take, and a room that isn't
        // there can't be given either.
        assert!(rooms.set_grant("Ryan", "Lounge", false).is_err());
        assert!(rooms.set_grant("Ryan", "Cellar", true).is_err());
        // Nor can somebody who was never invited be given a room.
        assert!(rooms.set_grant("Nobody", "Studio", true).is_err());
    }

    /// The owner has the house: every shared room, without a grant for
    /// any of them. Not somebody else's own room, though -- "just you
    /// and Vox" is a promise the room makes on its face.
    #[test]
    fn the_owner_has_the_house_but_not_a_room_of_somebody_elses() {
        let rooms = rooms(&["Lounge", "Studio"]);
        let boss = owner("Ryan");
        assert!(rooms.find(&boss, "Studio").is_some());
        assert_eq!(rooms.grants_of("Ryan").unwrap(), Vec::<String>::new());
        let ids: Vec<String> = rooms.directory(&boss)["rooms"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids, ["@Ryan", "Lounge", "Studio"]);
        assert!(rooms.find(&boss, "@Alex").is_none(), "not hers");
    }

    #[test]
    fn a_rooms_name_has_to_be_one_a_room_can_have() {
        assert_eq!(check_name("  Lounge ").unwrap(), "Lounge");
        assert!(check_name("").is_err());
        // `@` is how somebody's own room is named, `:` separates a room
        // from its settings in the database.
        assert!(check_name("@Ryan").is_err());
        assert!(check_name("a:b").is_err());
        assert!(check_name(&"x".repeat(NAME_MAX + 1)).is_err());
        // And two rooms cannot be the same room.
        let names = ["Lounge".to_string(), "lounge".to_string()];
        assert!(Rooms::new(
            Store::memory(),
            &base(),
            &names,
            Box::new(|_| unreachable!())
        )
        .is_err());
        assert!(Rooms::new(Store::memory(), &base(), &[], Box::new(|_| unreachable!())).is_err());
    }
}
