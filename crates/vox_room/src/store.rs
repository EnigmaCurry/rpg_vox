//! SQLite: who is invited, the conversation log, games, and room settings.
//!
//! ```sql
//! people(name, display, token, used, language)  -- an invite, and whose it is
//! sessions(id, name)                   -- a browser that traded its link in
//! entries(conversation, id, who, kind, title, text, cut, typed, translations, at)
//! settings(key, value)                 -- `ROOM:key` for one room's own
//! games(name, summary, prompt, settings, created, played)  -- names unique, ignoring case
//! conversations(id, room, game, at)       -- a room's history
//! cards(conversation, id, who, title, body, at)  -- the cards one person holds
//! ```
//!
//! Only finished entries are written: a person's paragraph once it has
//! been handed to the conversation (and again when a later pass revises
//! it), and the AI's reply once it has been spoken or cut off.

use std::path::Path;

use anyhow::{bail, Context as _, Result};
use rusqlite::{params, Connection, OptionalExtension as _};

use crate::room::{Card, Entry, GameSettings, Kind};

pub struct Store {
    db: Connection,
    /// Which room's settings this handle reads and writes. One server
    /// holds several rooms in one database, and what they have each
    /// been left set to -- the conversation they are in, how the AI
    /// takes its turn, where the faders are -- is one room's business.
    /// Empty is the whole database: what `vox_room invite` wants, and
    /// the scope the settings a room doesn't own are kept in.
    room: String,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS people (
    -- The key, and what `vox_room uninvite` names. It never changes.
    name  TEXT PRIMARY KEY,
    -- The one-time invite link. It stays here once spent, so a browser
    -- coming back to a dead link can be told that it is dead.
    token TEXT NOT NULL UNIQUE,
    -- Traded for a cookie: the link no longer works.
    used  INTEGER NOT NULL DEFAULT 0,
    -- What the room calls them, which is theirs to set at the door.
    display TEXT NOT NULL DEFAULT '',
    -- The language they speak and want to hear, spelled out in English
    -- as Qwen3-TTS wants it: Russian, not ru. Also set at the door.
    -- Empty: the room's own language. Kept against the name rather than
    -- the display name, so it survives their renaming themselves.
    language TEXT NOT NULL DEFAULT ''
);
-- A browser that pressed Join Audio: the cookie it was given, and whose
-- invite it spent. One person may have several (another browser, another
-- link); uninviting them drops the lot.
CREATE TABLE IF NOT EXISTS sessions (
    id   TEXT PRIMARY KEY,
    name TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS sessions_by_name ON sessions (name);
CREATE TABLE IF NOT EXISTS entries (
    conversation INTEGER NOT NULL,
    id   TEXT NOT NULL,
    who  TEXT NOT NULL,
    kind TEXT NOT NULL,
    -- A posted card's heading; empty for everything that was said.
    title TEXT NOT NULL DEFAULT '',
    text TEXT NOT NULL,
    cut  INTEGER NOT NULL DEFAULT 0,
    -- Typed into the room rather than said to it.
    typed INTEGER NOT NULL DEFAULT 0,
    -- What a typed message says in the other languages the room was
    -- listening in: a JSON object keyed by language. Kept so a reload
    -- shows what everyone was shown at the time.
    translations TEXT NOT NULL DEFAULT '{}',
    at   INTEGER NOT NULL,
    PRIMARY KEY (conversation, id)
);
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS games (
    name    TEXT PRIMARY KEY COLLATE NOCASE,
    summary TEXT NOT NULL,
    prompt  TEXT NOT NULL,
    created INTEGER NOT NULL,
    -- Entered at least once: from then on the game is fixed.
    played  INTEGER NOT NULL DEFAULT 0,
    -- The game's own settings, as JSON; see room::GameSettings.
    settings TEXT NOT NULL DEFAULT '{}'
);
-- A conversation a room has had. One row each, from the moment it is
-- started: a room's history is the rows with its name on them, and a
-- conversation with no room on it is one nobody can safely show.
CREATE TABLE IF NOT EXISTS conversations (
    id   INTEGER PRIMARY KEY,
    -- The room it belongs to. Empty only for a row left by a database
    -- written before conversations knew.
    room TEXT NOT NULL DEFAULT '',
    game TEXT NOT NULL,
    -- When it was started, in unix milliseconds. Zero on an old row,
    -- and then the first thing said in it stands in for it.
    at   INTEGER NOT NULL DEFAULT 0
);
-- A card the AI dealt to one person. Only they are ever sent it, and it
-- belongs to the conversation it was dealt in.
CREATE TABLE IF NOT EXISTS cards (
    conversation INTEGER NOT NULL,
    id    TEXT NOT NULL,
    who   TEXT NOT NULL,
    title TEXT NOT NULL,
    body  TEXT NOT NULL,
    at    INTEGER NOT NULL,
    PRIMARY KEY (conversation, id)
);
";

/// The number the next room to want a conversation is given, counted
/// for the whole database. See [`Store::claim_conversation`].
const CONVERSATION_COUNTER: &str = "conversations.next";
/// Set once [`Store::adopt`] has moved a pre-rooms database's settings
/// into the room they belonged to.
const ROOMS_FLAG: &str = "schema.rooms";

/// A saved game, without its prompt (which never leaves the server).
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct Game {
    pub name: String,
    pub summary: String,
    /// Unix milliseconds.
    pub created: u64,
    /// Entered at least once, and so fixed.
    pub played: bool,
    /// How the game wants the room run. Unlike the prompt, the browsers
    /// see this: it is the room's behaviour, not the game's secrets.
    pub settings: GameSettings,
}

/// One conversation a room has had, for its history.
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct Conversation {
    pub id: i64,
    /// The game it was played under, if it was one.
    pub game: Option<String>,
    /// Unix milliseconds. Zero when nothing was ever said in it and
    /// the row is old enough not to have been stamped.
    pub at: u64,
    /// How much was said in it.
    pub lines: usize,
    /// The first thing anybody said, for telling one from another.
    pub opening: String,
}

/// Somebody invited to the room.
#[derive(Debug, Clone, PartialEq)]
pub struct Invited {
    /// The key: what `vox_room uninvite` names, and what their cookie is
    /// tied to. It never changes.
    pub name: String,
    /// What the room calls them, theirs to set at the door. The same as
    /// `name` until they change it.
    pub display: String,
    /// Their invite link, while it is still unspent.
    pub link: Option<String>,
    /// The language they speak and want to hear, spelled out in English
    /// ("Russian"). Empty until they choose one at the door, which means
    /// the room's own language.
    pub language: String,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        }
        let db = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        db.execute_batch(SCHEMA)?;
        migrate(&db)?;
        // Several rooms are several connections to the one file, and
        // `vox_room invite` is a whole other process writing to it.
        // Wait for a writer rather than failing the read.
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Self {
            db,
            room: String::new(),
        })
    }

    #[cfg(test)]
    pub fn memory() -> Self {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(SCHEMA).unwrap();
        migrate(&db).unwrap();
        Self {
            db,
            room: String::new(),
        }
    }

    /// The same database as one room sees it: its own settings, and its
    /// own conversation.
    pub fn in_room(mut self, room: &str) -> Self {
        self.room = room.to_string();
        self
    }

    /// Where a setting of this room's lives. A room's name is in the
    /// key rather than a column of its own because the rooms a server
    /// holds are a matter of how it was started, not of what is in the
    /// database: a room that is no longer configured leaves its keys
    /// behind, harmlessly, and finds them again if it comes back.
    fn key(&self, key: &str) -> String {
        if self.room.is_empty() {
            key.to_string()
        } else {
            format!("{}:{}", self.room, key)
        }
    }

    /// `name`'s live invite link, made if they haven't got one.
    ///
    /// Asking twice gives the same link, so an invite can be printed
    /// again; asking for one whose link has already been traded in mints
    /// a second link rather than disturbing the browser holding the
    /// first (which is how someone gets onto a second device).
    pub fn invite(&self, name: &str) -> Result<String> {
        if let Some(t) = self
            .db
            .query_row(
                "SELECT token FROM people WHERE name = ?1 AND used = 0",
                [name],
                |r| r.get(0),
            )
            .optional()?
        {
            return Ok(t);
        }
        // The room calls people by their display name, so a new invite
        // can't take a name somebody else already answers to.
        if let Some(other) = self.person_called(name)? {
            if other != name {
                bail!("{other} is already in the room as {name}");
            }
        }
        let token = uuid::Uuid::new_v4().simple().to_string();
        self.db.execute(
            "INSERT INTO people (name, token, display) VALUES (?1, ?2, ?1)
             ON CONFLICT(name) DO UPDATE SET token = excluded.token, used = 0",
            params![name, token],
        )?;
        Ok(token)
    }

    /// Revoke `name`: their link stops working, and so does every cookie
    /// traded for one.
    pub fn uninvite(&self, name: &str) -> Result<bool> {
        let tx = self.db.unchecked_transaction()?;
        let gone = tx.execute("DELETE FROM people WHERE name = ?1", [name])? > 0;
        tx.execute("DELETE FROM sessions WHERE name = ?1", [name])?;
        tx.commit()?;
        Ok(gone)
    }

    /// Who this browser's cookie belongs to.
    pub fn person_for_session(&self, id: &str) -> Result<Option<Invited>> {
        Ok(self
            .db
            .query_row(
                "SELECT p.name, p.display, p.token, p.used, p.language FROM sessions s
                 JOIN people p ON p.name = s.name WHERE s.id = ?1",
                [id],
                invited_row,
            )
            .optional()?)
    }

    /// Forget one browser: the cookie stops working, and nobody else's
    /// does. Their invite stays spent, so they need a new link to come
    /// back.
    pub fn end_session(&self, id: &str) -> Result<bool> {
        Ok(self
            .db
            .execute("DELETE FROM sessions WHERE id = ?1", [id])?
            > 0)
    }

    /// The invite `token` opens, while it is still unspent.
    pub fn live_invite(&self, token: &str) -> Result<Option<Invited>> {
        Ok(self
            .db
            .query_row(
                "SELECT name, display, token, used, language FROM people
                 WHERE token = ?1 AND used = 0",
                [token],
                invited_row,
            )
            .optional()?)
    }

    /// Trade `token` for a cookie: the link is spent here and nowhere
    /// else. Returns the new session's id and whose it is, or None when
    /// the link had already been used (or never existed).
    pub fn claim_invite(&self, token: &str) -> Result<Option<(String, Invited)>> {
        let tx = self.db.unchecked_transaction()?;
        // One statement decides it, so two browsers racing on the same
        // link can't both win.
        if tx.execute(
            "UPDATE people SET used = 1 WHERE token = ?1 AND used = 0",
            [token],
        )? == 0
        {
            return Ok(None);
        }
        let invited: Invited = tx.query_row(
            "SELECT name, display, token, used, language FROM people WHERE token = ?1",
            [token],
            invited_row,
        )?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        tx.execute(
            "INSERT INTO sessions (id, name) VALUES (?1, ?2)",
            params![id, invited.name],
        )?;
        tx.commit()?;
        Ok(Some((id, invited)))
    }

    /// What `name` is called in the room. False when somebody else
    /// already answers to it.
    pub fn rename(&self, name: &str, display: &str) -> Result<bool> {
        if self.person_called(display)?.is_some_and(|who| who != name) {
            return Ok(false);
        }
        Ok(self.db.execute(
            "UPDATE people SET display = ?2 WHERE name = ?1",
            params![name, display],
        )? > 0)
    }

    /// The language `name` speaks and wants to hear. Empty for the
    /// room's own. False when nobody is invited under that name.
    pub fn set_language(&self, name: &str, language: &str) -> Result<bool> {
        Ok(self.db.execute(
            "UPDATE people SET language = ?2 WHERE name = ?1",
            params![name, language],
        )? > 0)
    }

    /// Whose display name this is, ignoring case. The way back from what
    /// the room calls somebody to the key everything is stored against.
    pub fn person_called(&self, display: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row(
                "SELECT name FROM people WHERE display = ?1 COLLATE NOCASE",
                [display],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn person(&self, name: &str) -> Result<Option<Invited>> {
        Ok(self
            .db
            .query_row(
                "SELECT name, display, token, used, language FROM people WHERE name = ?1",
                [name],
                invited_row,
            )
            .optional()?)
    }

    pub fn people(&self) -> Result<Vec<Invited>> {
        let mut q = self
            .db
            .prepare("SELECT name, display, token, used, language FROM people ORDER BY name")?;
        let rows = q.query_map([], invited_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// One of this room's settings.
    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        self.global(&self.key(key))
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.set_global(&self.key(key), value)
    }

    /// A setting of the whole database, whatever room this handle is
    /// for: the conversation counter every room draws from, and the
    /// flag that says the keys have been moved into rooms.
    fn global(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    fn set_global(&self, key: &str, value: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// The conversation this room is in. A room that has never had one
    /// is given a number of its own here, which is the only place a
    /// room's first conversation comes from.
    pub fn conversation(&self) -> Result<i64> {
        if let Some(id) = self.setting("conversation")?.and_then(|v| v.parse().ok()) {
            return Ok(id);
        }
        let id = self.claim_conversation()?;
        self.record_conversation(id, None)?;
        self.set_setting("conversation", &id.to_string())?;
        Ok(id)
    }

    /// Start a new, empty conversation here, in the lobby or playing
    /// `game`. The old one stays in the database.
    pub fn new_conversation(&self, game: Option<&str>) -> Result<i64> {
        let next = self.claim_conversation()?;
        self.record_conversation(next, game)?;
        self.set_setting("conversation", &next.to_string())?;
        Ok(next)
    }

    /// Write down that this room is having conversation `id`, from now.
    /// Every conversation, not only a game's: a room's history is the
    /// rows with its name on them, and one that was never written down
    /// is one the room can never show you again.
    fn record_conversation(&self, id: i64, game: Option<&str>) -> Result<()> {
        self.db.execute(
            "INSERT INTO conversations (id, room, game, at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET room = excluded.room, game = excluded.game",
            params![id, self.room, game.unwrap_or_default(), now_ms()],
        )?;
        Ok(())
    }

    /// Which room conversation `id` belongs to, if any room does. What
    /// says whether somebody may read it: a room you can be in is a
    /// conversation you can be shown.
    pub fn conversation_room(&self, id: i64) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row("SELECT room FROM conversations WHERE id = ?1", [id], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
            .filter(|room| !room.is_empty()))
    }

    /// Every conversation this room has had, newest first.
    pub fn history(&self) -> Result<Vec<Conversation>> {
        let mut q = self.db.prepare(
            "SELECT c.id, c.game,
                    COALESCE(NULLIF(c.at, 0),
                             (SELECT MIN(at) FROM entries e WHERE e.conversation = c.id),
                             0) AS at,
                    (SELECT COUNT(*) FROM entries e WHERE e.conversation = c.id) AS lines,
                    COALESCE((SELECT e.text FROM entries e
                               WHERE e.conversation = c.id
                                 AND e.kind IN ('human', 'agent')
                                 AND trim(e.text) <> ''
                               ORDER BY e.at, e.id LIMIT 1), '') AS opening
               FROM conversations c
              WHERE c.room = ?1
              ORDER BY at DESC, c.id DESC",
        )?;
        let rows = q.query_map([&self.room], |r| {
            Ok(Conversation {
                id: r.get(0)?,
                game: Some(r.get::<_, String>(1)?).filter(|g| !g.is_empty()),
                at: r.get::<_, i64>(2)? as u64,
                lines: r.get::<_, i64>(3)? as usize,
                opening: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Forget a conversation: everything said in it, every card dealt
    /// in it, and the row that says the room ever had it.
    pub fn forget_conversation(&self, id: i64) -> Result<()> {
        self.db
            .execute("DELETE FROM entries WHERE conversation = ?1", [id])?;
        self.db
            .execute("DELETE FROM cards WHERE conversation = ?1", [id])?;
        self.db
            .execute("DELETE FROM conversations WHERE id = ?1", [id])?;
        Ok(())
    }

    /// The next unused conversation number. Counted for the whole
    /// database rather than per room: entries and cards are keyed by
    /// the number alone, so a number handed to two rooms would be one
    /// log read by both.
    ///
    /// Past the counter *and* past everything already written, which is
    /// what makes the counter safe to be wrong: a database that has
    /// never had one (every database, before rooms), or one restored
    /// from a backup, can't hand out a number with somebody's words
    /// already in it. The rows are read off three primary keys and this
    /// happens when a person starts a conversation, so the cost of
    /// asking is nothing.
    fn claim_conversation(&self) -> Result<i64> {
        let counter = self
            .global(CONVERSATION_COUNTER)?
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        let used: i64 = self.db.query_row(
            "SELECT MAX(n) FROM (
                 SELECT COALESCE(MAX(conversation), 0) AS n FROM entries
                 UNION ALL SELECT COALESCE(MAX(conversation), 0) FROM cards
                 UNION ALL SELECT COALESCE(MAX(id), 0) FROM conversations)",
            [],
            |r| r.get(0),
        )?;
        let next = counter.max(used + 1).max(1);
        self.set_global(CONVERSATION_COUNTER, &(next + 1).to_string())?;
        Ok(next)
    }

    /// Take a database written before there was more than one room into
    /// `room`: the conversation it was in, how its AI took its turn and
    /// where its faders were belonged to the whole server, which was
    /// one room. Done once, and only to a database that has not been
    /// through it.
    pub fn adopt(&self, room: &str) -> Result<()> {
        if self.global(ROOMS_FLAG)?.is_some() {
            return Ok(());
        }
        for key in ["conversation", "mode", "strips"] {
            if let Some(value) = self.global(key)? {
                self.set_global(&format!("{room}:{key}"), &value)?;
                self.db
                    .execute("DELETE FROM settings WHERE key = ?1", [key])?;
            }
        }
        // The counter is left for `claim_conversation` to work out: it
        // reads the rows anyway, and so needs nothing set up here.
        self.set_global(ROOMS_FLAG, "1")?;
        Ok(())
    }

    /// The game `conversation` plays, if any.
    ///
    /// Every conversation has a row now, not only a game's, and a row
    /// with no game on it has an empty string there. An empty game is
    /// no game: without this the room comes up believing it is playing
    /// one called "", and goes looking for it.
    pub fn conversation_game(&self, conversation: i64) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row(
                "SELECT game FROM conversations WHERE id = ?1",
                [conversation],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .filter(|game| !game.is_empty()))
    }

    /// Every saved game, newest first.
    pub fn games(&self) -> Result<Vec<Game>> {
        let mut q = self.db.prepare(
            "SELECT name, summary, created, played, settings FROM games ORDER BY created DESC",
        )?;
        let rows = q.query_map([], game_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The game called `name` (matched ignoring case) and its prompt.
    pub fn game(&self, name: &str) -> Result<Option<(Game, String)>> {
        Ok(self
            .db
            .query_row(
                "SELECT name, summary, created, played, settings, prompt FROM games WHERE name = ?1",
                [name],
                |r| Ok((game_row(r)?, r.get(5)?)),
            )
            .optional()?)
    }

    pub fn mark_played(&self, name: &str) -> Result<()> {
        self.db
            .execute("UPDATE games SET played = 1 WHERE name = ?1", [name])?;
        Ok(())
    }

    pub fn delete_entry(&self, conversation: i64, id: &str) -> Result<()> {
        self.db.execute(
            "DELETE FROM entries WHERE conversation = ?1 AND id = ?2",
            params![conversation, id],
        )?;
        Ok(())
    }

    /// Save a game; false when the name (ignoring case) is taken.
    pub fn save_game(
        &self,
        name: &str,
        summary: &str,
        prompt: &str,
        settings: &GameSettings,
        created: u64,
    ) -> Result<bool> {
        Ok(self.db.execute(
            "INSERT OR IGNORE INTO games (name, summary, prompt, settings, created)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![name, summary, prompt, json(settings), created as i64],
        )? > 0)
    }

    /// Replace a game's name, summary and prompt, keeping when it was made
    /// and whether it has been played; its conversations follow a rename.
    pub fn update_game(
        &self,
        name: &str,
        new_name: &str,
        summary: &str,
        prompt: &str,
        settings: &GameSettings,
    ) -> Result<bool> {
        let changed = self.db.execute(
            "UPDATE games SET name = ?2, summary = ?3, prompt = ?4, settings = ?5 WHERE name = ?1",
            params![name, new_name, summary, prompt, json(settings)],
        )? > 0;
        if changed {
            self.db.execute(
                "UPDATE conversations SET game = ?2 WHERE game = ?1",
                params![name, new_name],
            )?;
        }
        Ok(changed)
    }

    pub fn delete_game(&self, name: &str) -> Result<bool> {
        Ok(self
            .db
            .execute("DELETE FROM games WHERE name = ?1", [name])?
            > 0)
    }

    pub fn put_entry(&self, conversation: i64, e: &Entry) -> Result<()> {
        self.db.execute(
            "INSERT INTO entries
                 (conversation, id, who, kind, title, text, cut, typed, translations, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(conversation, id) DO UPDATE SET
               text = excluded.text, cut = excluded.cut,
               translations = excluded.translations",
            params![
                conversation,
                e.id,
                e.who,
                e.kind.as_str(),
                e.title,
                e.text,
                e.cut as i64,
                e.typed as i64,
                serde_json::to_string(&e.translations).unwrap_or_else(|_| "{}".into()),
                e.at as i64
            ],
        )?;
        Ok(())
    }

    /// The last `limit` entries of `conversation`, oldest first.
    pub fn entries(&self, conversation: i64, limit: usize) -> Result<Vec<Entry>> {
        let mut q = self.db.prepare(
            "SELECT id, who, kind, title, text, cut, typed, translations, at FROM entries
             WHERE conversation = ?1 ORDER BY at DESC, rowid DESC LIMIT ?2",
        )?;
        let rows = q.query_map(params![conversation, limit as i64], |r| {
            let kind: String = r.get(2)?;
            let translations: String = r.get(7)?;
            Ok(Entry {
                id: r.get(0)?,
                who: r.get(1)?,
                kind: Kind::parse(&kind),
                title: r.get(3)?,
                text: r.get(4)?,
                live: false,
                partial: false,
                cut: r.get::<_, i64>(5)? != 0,
                typed: r.get::<_, i64>(6)? != 0,
                translations: serde_json::from_str(&translations).unwrap_or_default(),
                at: r.get::<_, i64>(8)? as u64,
                sent: true,
            })
        })?;
        let mut out: Vec<Entry> = rows.collect::<rusqlite::Result<_>>()?;
        out.reverse();
        Ok(out)
    }

    pub fn put_card(&self, conversation: i64, c: &Card) -> Result<()> {
        self.db.execute(
            "INSERT INTO cards (conversation, id, who, title, body, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![conversation, c.id, c.to, c.title, c.body, c.at as i64],
        )?;
        Ok(())
    }

    /// The cards `who` holds in `conversation`, oldest first.
    pub fn cards(&self, conversation: i64, who: &str) -> Result<Vec<Card>> {
        let mut q = self.db.prepare(
            "SELECT id, who, title, body, at FROM cards
             WHERE conversation = ?1 AND who = ?2 ORDER BY at, rowid",
        )?;
        let rows = q.query_map(params![conversation, who], |r| {
            Ok(Card {
                id: r.get(0)?,
                to: r.get(1)?,
                title: r.get(2)?,
                body: r.get(3)?,
                at: r.get::<_, i64>(4)? as u64,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// What each person is holding in `conversation`: who, and the
    /// card's title, oldest first.
    pub fn card_titles(&self, conversation: i64) -> Result<Vec<(String, String)>> {
        let mut q = self
            .db
            .prepare("SELECT who, title FROM cards WHERE conversation = ?1 ORDER BY at, rowid")?;
        let rows = q.query_map([conversation], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The card `who` is holding under `title`, matched ignoring case:
    /// dealing that title again replaces this one rather than stacking
    /// another copy beside it.
    pub fn card_titled(&self, conversation: i64, who: &str, title: &str) -> Result<Option<Card>> {
        Ok(self
            .cards(conversation, who)?
            .into_iter()
            .find(|c| c.title.eq_ignore_ascii_case(title.trim())))
    }

    /// Rewrite a card in place, keeping where it sits on screen.
    pub fn update_card(&self, conversation: i64, c: &Card) -> Result<()> {
        self.db.execute(
            "UPDATE cards SET title = ?3, body = ?4, at = ?5
             WHERE conversation = ?1 AND id = ?2",
            params![conversation, c.id, c.title, c.body, c.at as i64],
        )?;
        Ok(())
    }

    /// Take one card back; false when there was no such card.
    pub fn delete_card(&self, conversation: i64, id: &str) -> Result<bool> {
        Ok(self.db.execute(
            "DELETE FROM cards WHERE conversation = ?1 AND id = ?2",
            params![conversation, id],
        )? > 0)
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn invited_row(r: &rusqlite::Row) -> rusqlite::Result<Invited> {
    let name: String = r.get(0)?;
    let display: String = r.get(1)?;
    let token: String = r.get(2)?;
    let used: i64 = r.get(3)?;
    let language: String = r.get(4)?;
    Ok(Invited {
        // A row from before display names had their own column, or one
        // written by hand: the name is the name.
        display: if display.is_empty() {
            name.clone()
        } else {
            display
        },
        name,
        link: (used == 0).then_some(token),
        language,
    })
}

fn game_row(r: &rusqlite::Row) -> rusqlite::Result<Game> {
    let settings: String = r.get(4)?;
    Ok(Game {
        name: r.get(0)?,
        summary: r.get(1)?,
        created: r.get::<_, i64>(2)? as u64,
        played: r.get::<_, i64>(3)? != 0,
        // A game saved before a setting existed still loads: every field
        // defaults, so the worst case is the default settings.
        settings: serde_json::from_str(&settings).unwrap_or_default(),
    })
}

fn json(settings: &GameSettings) -> String {
    serde_json::to_string(settings).unwrap_or_else(|_| "{}".into())
}

/// Columns added after the first release. `CREATE TABLE IF NOT EXISTS`
/// does nothing to a table that is already there, so a database from
/// before a column existed needs it added.
fn migrate(db: &Connection) -> Result<()> {
    if !has_column(db, "games", "settings")? {
        db.execute_batch("ALTER TABLE games ADD COLUMN settings TEXT NOT NULL DEFAULT '{}'")?;
    }
    if !has_column(db, "entries", "title")? {
        db.execute_batch("ALTER TABLE entries ADD COLUMN title TEXT NOT NULL DEFAULT ''")?;
    }
    if !has_column(db, "people", "used")? {
        db.execute_batch("ALTER TABLE people ADD COLUMN used INTEGER NOT NULL DEFAULT 0")?;
    }
    // Invite links used to be reusable, and the cookie was the token
    // itself. Every browser already holding one keeps its place by
    // turning that token into a session; the link stays live as well,
    // because there is no way to tell who has used theirs.
    if !has_column(db, "people", "display")? {
        db.execute_batch(
            "ALTER TABLE people ADD COLUMN display TEXT NOT NULL DEFAULT '';
             UPDATE people SET display = name;
             INSERT OR IGNORE INTO sessions (id, name) SELECT token, name FROM people;",
        )?;
    }
    if !has_column(db, "people", "language")? {
        db.execute_batch("ALTER TABLE people ADD COLUMN language TEXT NOT NULL DEFAULT ''")?;
    }
    // Everything written before typing was possible was said out loud.
    if !has_column(db, "entries", "typed")? {
        db.execute_batch(
            "ALTER TABLE entries ADD COLUMN typed INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE entries ADD COLUMN translations TEXT NOT NULL DEFAULT '{}';",
        )?;
    }
    // Conversations didn't know which room they belonged to, and only a
    // game's was written down at all. What can be worked out is each
    // room's *current* one, which the settings name; the ones before it
    // can't be attributed to a room and so are left out of every
    // room's history rather than guessed into one.
    if !has_column(db, "conversations", "room")? {
        db.execute_batch(
            "ALTER TABLE conversations ADD COLUMN room TEXT NOT NULL DEFAULT '';
             ALTER TABLE conversations ADD COLUMN at INTEGER NOT NULL DEFAULT 0;
             INSERT OR IGNORE INTO conversations (id, room, game, at)
                 SELECT CAST(value AS INTEGER),
                        substr(key, 1, length(key) - length(':conversation')),
                        '', 0
                   FROM settings WHERE key LIKE '%:conversation';
             UPDATE conversations SET room = COALESCE((
                     SELECT substr(s.key, 1, length(s.key) - length(':conversation'))
                       FROM settings s
                      WHERE s.key LIKE '%:conversation'
                        AND CAST(s.value AS INTEGER) = conversations.id), '')
               WHERE room = '';",
        )?;
    }
    Ok(())
}

fn has_column(db: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut q = db.prepare("SELECT name FROM pragma_table_info(?1)")?;
    let mut rows = q.query([table])?;
    while let Some(r) = rows.next()? {
        if r.get::<_, String>(0)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn plain() -> GameSettings {
        GameSettings::default()
    }

    #[test]
    fn an_invite_link_works_once_and_leaves_a_session() {
        let s = Store::memory();
        let link = s.invite("Ryan").unwrap();
        assert_eq!(s.invite("Ryan").unwrap(), link, "printing it again");
        assert_eq!(
            s.live_invite(&link).unwrap().unwrap().name,
            "Ryan",
            "unspent, so the door opens"
        );

        let (cookie, who) = s.claim_invite(&link).unwrap().unwrap();
        assert_eq!(who.display, "Ryan");
        assert_eq!(s.live_invite(&link).unwrap(), None, "spent");
        assert!(s.claim_invite(&link).unwrap().is_none(), "and only once");
        assert_eq!(
            s.person_for_session(&cookie).unwrap().unwrap().name,
            "Ryan",
            "but the cookie it was traded for lasts"
        );

        // Logging out forgets this browser and nothing else.
        let (other, _) = s.claim_invite(&s.invite("Ryan").unwrap()).unwrap().unwrap();
        assert!(s.end_session(&cookie).unwrap());
        assert_eq!(s.person_for_session(&cookie).unwrap(), None);
        assert!(s.person_for_session(&other).unwrap().is_some());
        assert!(!s.end_session(&cookie).unwrap());
        let (cookie, _) = s.claim_invite(&s.invite("Ryan").unwrap()).unwrap().unwrap();
        assert_eq!(s.person("Ryan").unwrap().unwrap().link, None);

        // Asking again mints a second link, leaving the cookie alone.
        let again = s.invite("Ryan").unwrap();
        assert_ne!(again, link);
        assert!(s.person_for_session(&cookie).unwrap().is_some());

        // Uninvite takes the link and the cookie.
        assert!(s.uninvite("Ryan").unwrap());
        assert_eq!(s.live_invite(&again).unwrap(), None);
        assert_eq!(s.person_for_session(&cookie).unwrap(), None);
        assert!(!s.uninvite("Ryan").unwrap());
    }

    #[test]
    fn a_display_name_is_theirs_alone_and_the_key_stays_put() {
        let s = Store::memory();
        let link = s.invite("Mike").unwrap();
        s.invite("Alex").unwrap();

        assert!(s.rename("Mike", "Mikey").unwrap());
        assert_eq!(s.live_invite(&link).unwrap().unwrap().display, "Mikey");
        assert_eq!(
            s.person("Mike").unwrap().unwrap().name,
            "Mike",
            "the key for uninvite doesn't move"
        );
        assert!(!s.rename("Mike", "alex").unwrap(), "Alex answers to that");
        assert!(s.rename("Mike", "MIKEY").unwrap(), "their own, recased");

        // ...nor can a new invite take a name somebody answers to.
        assert!(s.invite("mikey").is_err());
        assert!(s.uninvite("Mike").unwrap());
        assert!(s.invite("mikey").is_ok());
    }

    #[test]
    fn a_language_is_remembered_against_the_key_not_the_display_name() {
        let s = Store::memory();
        let link = s.invite("Mike").unwrap();
        assert_eq!(
            s.person("Mike").unwrap().unwrap().language,
            "",
            "nobody has chosen one yet"
        );

        assert!(s.set_language("Mike", "Russian").unwrap());
        assert_eq!(s.person("Mike").unwrap().unwrap().language, "Russian");
        // Every way of reading a person back carries it, because each
        // SELECT has its own column list.
        assert_eq!(s.live_invite(&link).unwrap().unwrap().language, "Russian");
        assert_eq!(s.people().unwrap()[0].language, "Russian");

        // Renaming themselves at the door doesn't lose it: the language
        // hangs off the name, which never moves.
        assert!(s.rename("Mike", "Mikey").unwrap());
        assert_eq!(s.person("Mike").unwrap().unwrap().language, "Russian");

        let (id, invited) = s.claim_invite(&link).unwrap().unwrap();
        assert_eq!(invited.language, "Russian");
        assert_eq!(
            s.person_for_session(&id).unwrap().unwrap().language,
            "Russian",
            "and the browser that spent the link sees it too"
        );

        // Back to the room's own language.
        assert!(s.set_language("Mike", "").unwrap());
        assert_eq!(s.person("Mike").unwrap().unwrap().language, "");
        assert!(!s.set_language("Nobody", "Russian").unwrap());
    }

    /// A database on disk, since two rooms are two connections to one
    /// file and that is the thing being tested. Dropped with the guard.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(what: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "vox_room_test_{}_{what}_{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir.join("room.db"))
        }

        fn open(&self, room: &str) -> Store {
            Store::open(&self.0).unwrap().in_room(room)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            if let Some(dir) = self.0.parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

    /// Same file, two rooms: what is one room's stays one room's, and
    /// what is everybody's is read by both.
    #[test]
    fn each_room_has_its_own_settings_and_its_own_conversation() {
        let scratch = Scratch::new("rooms");
        let lounge = scratch.open("Lounge");
        let studio = scratch.open("Studio");
        lounge.invite("Mike").unwrap();

        // A conversation each, and never the same one.
        let a = lounge.conversation().unwrap();
        let b = studio.conversation().unwrap();
        assert_ne!(a, b, "two rooms must never share a log");
        assert_eq!(lounge.conversation().unwrap(), a, "and keep it");
        assert_eq!(studio.conversation().unwrap(), b);
        assert_ne!(
            lounge.new_conversation(None).unwrap(),
            b,
            "nor land on one in use"
        );

        // A setting is the room's.
        lounge.set_setting("mode", "always").unwrap();
        assert_eq!(lounge.setting("mode").unwrap().as_deref(), Some("always"));
        assert_eq!(studio.setting("mode").unwrap(), None, "not the Studio's");

        // The invite list is everybody's.
        assert_eq!(studio.people().unwrap().len(), 1);
        assert_eq!(lounge.people().unwrap().len(), 1);
    }

    /// A room keeps what it has said. Every conversation is written
    /// down from the moment it starts, with the room's name on it, so
    /// the room can show it again -- and one with no game on it is not
    /// a conversation playing a game called "".
    #[test]
    fn a_room_keeps_the_conversations_it_has_had() {
        let scratch = Scratch::new("history");
        let lounge = scratch.open("Lounge");
        let studio = scratch.open("Studio");

        let first = lounge.conversation().unwrap();
        assert_eq!(
            lounge.conversation_game(first).unwrap(),
            None,
            "an empty game is no game"
        );
        let e = |id: &str, text: &str| Entry {
            id: id.into(),
            who: "Ryan".into(),
            kind: Kind::Human,
            title: String::new(),
            text: text.into(),
            live: false,
            partial: false,
            cut: false,
            typed: false,
            translations: BTreeMap::new(),
            at: 10,
            sent: true,
        };
        lounge
            .put_entry(first, &e("a", "the first conversation"))
            .unwrap();

        // A new one closes the old; both are the room's, newest first.
        let second = lounge.new_conversation(None).unwrap();
        lounge.put_entry(second, &e("b", "the second")).unwrap();
        let history = lounge.history().unwrap();
        assert_eq!(
            history.iter().map(|c| c.id).collect::<Vec<_>>(),
            [second, first]
        );
        assert_eq!(history[1].opening, "the first conversation");
        assert_eq!(history[1].lines, 1);
        assert!(history[0].at > 0, "stamped when it was started");

        // A game is one of them, and says so.
        let game = lounge.new_conversation(Some("Quick Draw")).unwrap();
        assert_eq!(
            lounge.conversation_game(game).unwrap().as_deref(),
            Some("Quick Draw")
        );
        assert_eq!(
            lounge.history().unwrap()[0].game.as_deref(),
            Some("Quick Draw")
        );

        // Another room's history is its own, and so is the question of
        // whose a conversation is.
        studio.conversation().unwrap();
        assert_eq!(studio.history().unwrap().len(), 1);
        assert_eq!(
            lounge.conversation_room(first).unwrap().as_deref(),
            Some("Lounge")
        );

        // Forgetting one takes everything said in it with it.
        lounge.forget_conversation(first).unwrap();
        assert!(lounge.entries(first, 10).unwrap().is_empty());
        assert_eq!(lounge.conversation_room(first).unwrap(), None);
        assert_eq!(
            lounge
                .history()
                .unwrap()
                .iter()
                .map(|c| c.id)
                .collect::<Vec<_>>(),
            [game, second]
        );
    }

    /// A database from when the server was one room: what was the whole
    /// server's belongs to the room that was there, and the rooms made
    /// afterwards must not be handed its conversation.
    #[test]
    fn a_database_from_before_rooms_is_adopted_by_the_first_one() {
        let scratch = Scratch::new("adopt");
        {
            // As it was written: unprefixed keys, and entries under the
            // conversation number they name.
            let old = scratch.open("");
            old.set_global("conversation", "4").unwrap();
            old.set_global("mode", "auto").unwrap();
            old.set_global("strips", "{}").unwrap();
            old.db
                .execute(
                    "INSERT INTO entries (conversation, id, who, kind, text, at)
                     VALUES (4, 'e1', 'Mike', 'human', 'hello', 1)",
                    [],
                )
                .unwrap();
        }

        scratch.open("").adopt("Lobby").unwrap();
        let main = scratch.open("Lobby");
        assert_eq!(main.conversation().unwrap(), 4, "the room it was left in");
        assert_eq!(main.setting("mode").unwrap().as_deref(), Some("auto"));
        assert_eq!(main.setting("strips").unwrap().as_deref(), Some("{}"));
        assert_eq!(main.entries(4, 10).unwrap().len(), 1);

        // A room made afterwards gets a number of its own, past
        // everything already written.
        let other = scratch.open("Studio");
        let theirs = other.conversation().unwrap();
        assert!(theirs > 4, "past everything already written");
        assert!(other.entries(theirs, 10).unwrap().is_empty());

        // And adopting again does nothing: it has been through it once.
        scratch.open("").adopt("Studio").unwrap();
        assert_eq!(
            scratch.open("Studio").setting("mode").unwrap(),
            None,
            "the Studio never had a mode of its own"
        );
    }

    #[test]
    fn a_database_from_before_languages_opens_and_gains_the_column() {
        // `people` as it was written before the column existed. SCHEMA
        // is CREATE TABLE IF NOT EXISTS, so it leaves this one alone and
        // only `migrate` can rescue it.
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE people (
                 name TEXT PRIMARY KEY,
                 token TEXT NOT NULL UNIQUE,
                 used INTEGER NOT NULL DEFAULT 0,
                 display TEXT NOT NULL DEFAULT ''
             );
             INSERT INTO people (name, token, used, display)
                 VALUES ('Mike', 'tok', 1, 'Mikey');",
        )
        .unwrap();
        db.execute_batch(SCHEMA).unwrap();
        migrate(&db).unwrap();
        let s = Store {
            db,
            room: String::new(),
        };

        let mike = s.person("Mike").unwrap().unwrap();
        assert_eq!(mike.display, "Mikey");
        assert_eq!(mike.language, "", "the room's own language, as before");
        assert!(s.set_language("Mike", "Russian").unwrap());
        assert_eq!(s.person("Mike").unwrap().unwrap().language, "Russian");
    }

    #[test]
    fn entries_round_trip_in_order() {
        let s = Store::memory();
        let e = |id: &str, at| Entry {
            id: id.into(),
            who: "Ryan".into(),
            kind: Kind::Human,
            title: String::new(),
            text: format!("text {id}"),
            live: false,
            partial: false,
            cut: false,
            typed: false,
            translations: BTreeMap::new(),
            at,
            sent: true,
        };
        s.put_entry(1, &e("b", 20)).unwrap();
        s.put_entry(1, &e("a", 10)).unwrap();
        s.put_entry(2, &e("c", 30)).unwrap();
        let mut revised = e("a", 10);
        revised.text = "revised".into();
        s.put_entry(1, &revised).unwrap();
        let got = s.entries(1, 10).unwrap();
        assert_eq!(
            got.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(got[0].text, "revised");
        // Past both the conversations already written to: a number
        // handed out twice would be one log two rooms are reading.
        assert_eq!(s.new_conversation(None).unwrap(), 3);
        assert_eq!(s.entries(2, 10).unwrap().len(), 1);

        // A typed message keeps its mark and its translations, so coming
        // back to a conversation shows what everyone was shown then. The
        // translations arrive after the first write, so the update path
        // has to carry them too.
        let mut t = e("typed-1", 40);
        t.typed = true;
        s.put_entry(1, &t).unwrap();
        t.translations
            .insert("Russian".into(), "см. https://example.com/a_b".into());
        s.put_entry(1, &t).unwrap();
        let back = s.entries(1, 10).unwrap();
        let back = back.iter().find(|e| e.id == "typed-1").unwrap();
        assert!(back.typed);
        assert_eq!(
            back.translations.get("Russian").map(String::as_str),
            Some("см. https://example.com/a_b")
        );
        // And everything said aloud reads back as it always did.
        assert!(!s
            .entries(1, 10)
            .unwrap()
            .iter()
            .any(|e| e.id == "a" && (e.typed || !e.translations.is_empty())));
    }

    #[test]
    fn games_have_unique_names_and_stay_with_their_conversations() {
        let s = Store::memory();
        assert!(s
            .save_game("Dragon Heist", "A heist.", "secret", &plain(), 1)
            .unwrap());
        assert!(!s
            .save_game("dragon heist", "Again.", "other", &plain(), 2)
            .unwrap());
        assert!(s.save_game("Zork", "Caves.", "dark", &plain(), 3).unwrap());
        assert_eq!(
            s.games()
                .unwrap()
                .iter()
                .map(|g| g.name.as_str())
                .collect::<Vec<_>>(),
            ["Zork", "Dragon Heist"]
        );
        let (g, prompt) = s.game("DRAGON HEIST").unwrap().unwrap();
        assert_eq!(
            (g.name.as_str(), prompt.as_str(), g.played),
            ("Dragon Heist", "secret", false)
        );
        s.mark_played("dragon heist").unwrap();
        assert!(s.game("Dragon Heist").unwrap().unwrap().0.played);
        let c = s.new_conversation(Some("Zork")).unwrap();
        assert_eq!(s.conversation_game(c).unwrap().as_deref(), Some("Zork"));
        let lobby = s.new_conversation(None).unwrap();
        assert_eq!(s.conversation_game(lobby).unwrap(), None);
        assert!(s.delete_game("zork").unwrap());
        assert_eq!(s.games().unwrap().len(), 1);
    }
}
