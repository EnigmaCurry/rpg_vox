//! SQLite: who is invited, the conversation log, and room settings.
//!
//! ```sql
//! people(name, token)
//! entries(conversation, id, who, kind, text, cut, at)
//! settings(key, value)
//! ```
//!
//! Only finished entries are written: a person's paragraph once it has
//! been handed to the conversation (and again when a later pass revises
//! it), and the AI's reply once it has been spoken or cut off.

use std::path::Path;

use anyhow::{Context as _, Result};
use rusqlite::{params, Connection, OptionalExtension as _};

use crate::room::{Entry, Kind};

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
    text TEXT NOT NULL,
    cut  INTEGER NOT NULL DEFAULT 0,
    at   INTEGER NOT NULL,
    PRIMARY KEY (conversation, id)
);
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        }
        let db = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        db.execute_batch(SCHEMA)?;
        Ok(Self { db })
    }

    #[cfg(test)]
    pub fn memory() -> Self {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(SCHEMA).unwrap();
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

    /// Start a new, empty conversation. The old one stays in the database.
    pub fn new_conversation(&self) -> Result<i64> {
        let next = self.conversation()? + 1;
        self.set_setting("conversation", &next.to_string())?;
        Ok(next)
    }

    pub fn put_entry(&self, conversation: i64, e: &Entry) -> Result<()> {
        self.db.execute(
            "INSERT INTO entries (conversation, id, who, kind, text, cut, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(conversation, id) DO UPDATE SET
               text = excluded.text, cut = excluded.cut",
            params![
                conversation,
                e.id,
                e.who,
                e.kind.as_str(),
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
            "SELECT id, who, kind, text, cut, at FROM entries
             WHERE conversation = ?1 ORDER BY at DESC, rowid DESC LIMIT ?2",
        )?;
        let rows = q.query_map(params![conversation, limit as i64], |r| {
            let kind: String = r.get(2)?;
            Ok(Entry {
                id: r.get(0)?,
                who: r.get(1)?,
                kind: if kind == "agent" {
                    Kind::Agent
                } else {
                    Kind::Human
                },
                text: r.get(3)?,
                live: false,
                partial: false,
                cut: r.get::<_, i64>(4)? != 0,
                at: r.get::<_, i64>(5)? as u64,
                sent: true,
            })
        })?;
        let mut out: Vec<Entry> = rows.collect::<rusqlite::Result<_>>()?;
        out.reverse();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(s.new_conversation().unwrap(), 2);
        assert_eq!(s.entries(2, 10).unwrap().len(), 1);
    }
}
