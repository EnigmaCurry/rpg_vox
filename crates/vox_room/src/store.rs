//! SQLite: who is invited, the conversation log, games, and room settings.
//!
//! ```sql
//! people(name, token)
//! entries(conversation, id, who, kind, title, text, cut, at)
//! settings(key, value)
//! games(name, summary, prompt, settings, created, played)  -- names unique, ignoring case
//! conversations(id, game)                 -- no row: the lobby
//! cards(conversation, id, who, title, body, at)  -- one person's private cards
//! ```
//!
//! Only finished entries are written: a person's paragraph once it has
//! been handed to the conversation (and again when a later pass revises
//! it), and the AI's reply once it has been spoken or cut off.

use std::path::Path;

use anyhow::{Context as _, Result};
use rusqlite::{params, Connection, OptionalExtension as _};

use crate::room::{Card, Entry, GameSettings, Kind};

pub struct Store {
    db: Connection,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS people (
    name  TEXT PRIMARY KEY,
    token TEXT NOT NULL UNIQUE
);
CREATE TABLE IF NOT EXISTS entries (
    conversation INTEGER NOT NULL,
    id   TEXT NOT NULL,
    who  TEXT NOT NULL,
    kind TEXT NOT NULL,
    -- A posted card's heading; empty for everything that was said.
    title TEXT NOT NULL DEFAULT '',
    text TEXT NOT NULL,
    cut  INTEGER NOT NULL DEFAULT 0,
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
CREATE TABLE IF NOT EXISTS conversations (
    id   INTEGER PRIMARY KEY,
    game TEXT NOT NULL
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

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        }
        let db = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        db.execute_batch(SCHEMA)?;
        migrate(&db)?;
        Ok(Self { db })
    }

    #[cfg(test)]
    pub fn memory() -> Self {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(SCHEMA).unwrap();
        migrate(&db).unwrap();
        Self { db }
    }

    /// The invite token for `name`, made on first use.
    pub fn invite(&self, name: &str) -> Result<String> {
        if let Some(t) = self
            .db
            .query_row("SELECT token FROM people WHERE name = ?1", [name], |r| {
                r.get(0)
            })
            .optional()?
        {
            return Ok(t);
        }
        let token = uuid::Uuid::new_v4().simple().to_string();
        self.db.execute(
            "INSERT INTO people (name, token) VALUES (?1, ?2)",
            params![name, token],
        )?;
        Ok(token)
    }

    pub fn uninvite(&self, name: &str) -> Result<bool> {
        Ok(self
            .db
            .execute("DELETE FROM people WHERE name = ?1", [name])?
            > 0)
    }

    pub fn person_for_token(&self, token: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row("SELECT name FROM people WHERE token = ?1", [token], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn people(&self) -> Result<Vec<(String, String)>> {
        let mut q = self
            .db
            .prepare("SELECT name, token FROM people ORDER BY name")?;
        let rows = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// The conversation the room is in; the first one is 1.
    pub fn conversation(&self) -> Result<i64> {
        Ok(self
            .setting("conversation")?
            .and_then(|v| v.parse().ok())
            .unwrap_or(1))
    }

    /// Start a new, empty conversation, in the lobby or playing `game`.
    /// The old one stays in the database.
    pub fn new_conversation(&self, game: Option<&str>) -> Result<i64> {
        let next = self.conversation()? + 1;
        self.set_setting("conversation", &next.to_string())?;
        if let Some(g) = game {
            self.db.execute(
                "INSERT OR REPLACE INTO conversations (id, game) VALUES (?1, ?2)",
                params![next, g],
            )?;
        }
        Ok(next)
    }

    /// The game `conversation` plays, if any.
    pub fn conversation_game(&self, conversation: i64) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row(
                "SELECT game FROM conversations WHERE id = ?1",
                [conversation],
                |r| r.get(0),
            )
            .optional()?)
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
            "INSERT INTO entries (conversation, id, who, kind, title, text, cut, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(conversation, id) DO UPDATE SET
               text = excluded.text, cut = excluded.cut",
            params![
                conversation,
                e.id,
                e.who,
                e.kind.as_str(),
                e.title,
                e.text,
                e.cut as i64,
                e.at as i64
            ],
        )?;
        Ok(())
    }

    /// The last `limit` entries of `conversation`, oldest first.
    pub fn entries(&self, conversation: i64, limit: usize) -> Result<Vec<Entry>> {
        let mut q = self.db.prepare(
            "SELECT id, who, kind, title, text, cut, at FROM entries
             WHERE conversation = ?1 ORDER BY at DESC, rowid DESC LIMIT ?2",
        )?;
        let rows = q.query_map(params![conversation, limit as i64], |r| {
            let kind: String = r.get(2)?;
            Ok(Entry {
                id: r.get(0)?,
                who: r.get(1)?,
                kind: Kind::parse(&kind),
                title: r.get(3)?,
                text: r.get(4)?,
                live: false,
                partial: false,
                cut: r.get::<_, i64>(5)? != 0,
                at: r.get::<_, i64>(6)? as u64,
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
    let mut q = db.prepare("SELECT name FROM pragma_table_info('games')")?;
    let columns: Vec<String> = q
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if !columns.iter().any(|c| c == "settings") {
        db.execute_batch("ALTER TABLE games ADD COLUMN settings TEXT NOT NULL DEFAULT '{}'")?;
    }
    let mut q = db.prepare("SELECT name FROM pragma_table_info('entries')")?;
    let columns: Vec<String> = q
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if !columns.iter().any(|c| c == "title") {
        db.execute_batch("ALTER TABLE entries ADD COLUMN title TEXT NOT NULL DEFAULT ''")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> GameSettings {
        GameSettings::default()
    }

    #[test]
    fn invites_are_stable_and_revocable() {
        let s = Store::memory();
        let t = s.invite("Ryan").unwrap();
        assert_eq!(s.invite("Ryan").unwrap(), t);
        assert_eq!(s.person_for_token(&t).unwrap().as_deref(), Some("Ryan"));
        assert!(s.uninvite("Ryan").unwrap());
        assert_eq!(s.person_for_token(&t).unwrap(), None);
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
        assert_eq!(s.new_conversation(None).unwrap(), 2);
        assert_eq!(s.entries(2, 10).unwrap().len(), 1);
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
