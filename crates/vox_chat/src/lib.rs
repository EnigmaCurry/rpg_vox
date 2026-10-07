//! The `scribe --chat` database: a table of messages that scribe and a
//! responder (`agent`, or `scribe chat-echo`) both write. Neither side is
//! notified of the other's writes; each polls for rows after the last
//! id it has seen, which is cheap and well under the time a reply takes.
//!
//! ```sql
//! messages(id, conversation, role, kind, reply_to, text, more, at)
//! conversations(id, title, created_at)
//! settings(key, value)
//! ```
//!
//! One database holds any number of conversations; every message belongs
//! to one. A reply or interrupt belongs to the conversation of the user
//! message it refers to (`reply_to`), filled in by [`Db::reply`] and
//! [`Db::interrupt`], so a responder needn't track conversations.
//!
//! * `role = 'user', kind = 'say'`: something the user said and sent
//!   with Enter.
//! * `role = 'assistant', kind = 'say'`: part of the reply to user row
//!   `reply_to`, spoken in id order. A reply may come in any number of
//!   rows (a sentence at a time, or several separate utterances); every
//!   row but the last has `more = 1`, and scribe keeps waiting for the
//!   rest. The last has `more = 0`, and scribe listens again once it has
//!   been spoken. An empty row with `more = 1` means "thinking": scribe
//!   shows a spinner until text (or `more = 0`) arrives. A responder
//!   should send one as soon as it starts on a reply.
//! * `role = 'user', kind = 'interrupt'`: the user cut off the reply to
//!   `reply_to` (by talking over it). The responder should stop writing
//!   that reply; scribe ignores any more rows for it. A new `say` from
//!   the user also supersedes any reply still in progress.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result};
use rusqlite::{params, Connection, OptionalExtension};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS conversations (
    id         INTEGER PRIMARY KEY,
    title      TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE TABLE IF NOT EXISTS messages (
    id       INTEGER PRIMARY KEY,
    conversation INTEGER REFERENCES conversations(id),
    role     TEXT    NOT NULL CHECK (role IN ('user', 'assistant')),
    kind     TEXT    NOT NULL DEFAULT 'say' CHECK (kind IN ('say', 'interrupt')),
    reply_to INTEGER REFERENCES messages(id),
    text     TEXT    NOT NULL DEFAULT '',
    more     INTEGER NOT NULL DEFAULT 0,
    at       TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
-- Choices kept with the database: `conversation` (the one last open),
-- `voice.<conversation id>` (the Kokoro voice of its replies).
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Say,
    Interrupt,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub id: i64,
    pub conversation: i64,
    pub role: Role,
    pub kind: Kind,
    pub reply_to: Option<i64>,
    pub text: String,
    pub more: bool,
    /// When it was written, UTC: `2026-10-06T17:09:24.123Z`.
    pub at: String,
}

/// `NAME.db`, unless NAME already ends in `.db`.
pub fn path_for(name: &Path) -> PathBuf {
    if name
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("db"))
    {
        return name.to_path_buf();
    }
    let mut s = name.as_os_str().to_owned();
    s.push(".db");
    PathBuf::from(s)
}

/// A conversation, for listing.
#[derive(Debug, Clone, PartialEq)]
pub struct Conversation {
    pub id: i64,
    pub title: String,
    /// Its last message (or its creation), local time, `2026-10-06 17:09`.
    pub last: String,
    pub messages: i64,
}

/// A new conversation's title until it is renamed: the local date and time.
const NOW_TITLE: &str = "strftime('%Y-%m-%d %H:%M', 'now', 'localtime')";

pub struct Db {
    conn: Connection,
}

impl Db {
    /// Open (or create) the database. WAL lets the responder read while
    /// scribe writes; the busy timeout covers the moments both write.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// Start a conversation, titled with the date. Returns its id.
    pub fn new_conversation(&self) -> Result<i64> {
        self.conn.execute(
            &format!("INSERT INTO conversations (title) VALUES ({NOW_TITLE})"),
            [],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn rename(&self, conversation: i64, title: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE conversations SET title = ?2 WHERE id = ?1",
            params![conversation, title],
        )?;
        Ok(())
    }

    pub fn title(&self, conversation: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT title FROM conversations WHERE id = ?1",
                [conversation],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Every conversation, the most recently active first.
    pub fn conversations(&self) -> Result<Vec<Conversation>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.title,
                    strftime('%Y-%m-%d %H:%M', coalesce(max(m.at), c.created_at), 'localtime')
                        AS last,
                    count(m.id)
             FROM conversations c LEFT JOIN messages m ON m.conversation = c.id
             GROUP BY c.id
             ORDER BY coalesce(max(m.at), c.created_at) DESC, c.id DESC",
        )?;
        let list = stmt
            .query_map([], |r| {
                Ok(Conversation {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    last: r.get(2)?,
                    messages: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(list)
    }

    /// The conversation last open (see [`Db::set_current`]), else the
    /// most recent, else a new one.
    pub fn current(&self) -> Result<i64> {
        let saved = self
            .setting("conversation")?
            .and_then(|v| v.parse::<i64>().ok());
        if let Some(id) = saved {
            if self.title(id)?.is_some() {
                return Ok(id);
            }
        }
        match self.conversations()?.first() {
            Some(c) => Ok(c.id),
            None => self.new_conversation(),
        }
    }

    /// Remember `conversation` as the one to open next time.
    pub fn set_current(&self, conversation: i64) -> Result<()> {
        self.set_setting("conversation", &conversation.to_string())
    }

    /// The user said `text` in `conversation`. Returns its id.
    pub fn say(&self, conversation: i64, text: &str) -> Result<i64> {
        self.insert(Some(conversation), Role::User, Kind::Say, None, text, false)
    }

    /// The user cut off the reply to `reply_to`.
    pub fn interrupt(&self, reply_to: i64) -> Result<i64> {
        self.insert(None, Role::User, Kind::Interrupt, Some(reply_to), "", false)
    }

    /// Part of the reply to `reply_to`; `more` if the rest is coming.
    pub fn reply(&self, reply_to: i64, text: &str, more: bool) -> Result<i64> {
        self.insert(None, Role::Assistant, Kind::Say, Some(reply_to), text, more)
    }

    /// `conversation` defaults to that of `reply_to`.
    fn insert(
        &self,
        conversation: Option<i64>,
        role: Role,
        kind: Kind,
        reply_to: Option<i64>,
        text: &str,
        more: bool,
    ) -> Result<i64> {
        let role = match role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        let kind = match kind {
            Kind::Say => "say",
            Kind::Interrupt => "interrupt",
        };
        self.conn.execute(
            "INSERT INTO messages (conversation, role, kind, reply_to, text, more)
             VALUES (coalesce(?1, (SELECT conversation FROM messages WHERE id = ?3)),
                     ?2, ?4, ?3, ?5, ?6)",
            params![conversation, role, reply_to, kind, text, more as i64],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Rows after `id`, in every conversation, oldest first.
    pub fn since(&self, id: i64) -> Result<Vec<Row>> {
        self.rows("WHERE id > ?1", id)
    }

    /// Every row of `conversation`, oldest first.
    pub fn rows_in(&self, conversation: i64) -> Result<Vec<Row>> {
        self.rows("WHERE conversation = ?1", conversation)
    }

    fn rows(&self, filter: &str, arg: i64) -> Result<Vec<Row>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT id, coalesce(conversation, 0), role, kind, reply_to, text, more, at
             FROM messages {filter} ORDER BY id"
        ))?;
        let rows = stmt
            .query_map([arg], |r| {
                Ok(Row {
                    id: r.get(0)?,
                    conversation: r.get(1)?,
                    role: if r.get::<_, String>(2)? == "assistant" {
                        Role::Assistant
                    } else {
                        Role::User
                    },
                    kind: if r.get::<_, String>(3)? == "interrupt" {
                        Kind::Interrupt
                    } else {
                        Kind::Say
                    },
                    reply_to: r.get(4)?,
                    text: r.get(5)?,
                    more: r.get::<_, i64>(6)? != 0,
                    at: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// A setting kept with the database.
    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn last_id(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT max(id) FROM messages", [], |r| {
                r.get::<_, Option<i64>>(0)
            })
            .optional()?
            .flatten()
            .unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_get_db() {
        assert_eq!(path_for(Path::new("talk")), PathBuf::from("talk.db"));
        assert_eq!(path_for(Path::new("talk.db")), PathBuf::from("talk.db"));
        assert_eq!(path_for(Path::new("a.b")), PathBuf::from("a.b.db"));
    }

    #[test]
    fn round_trips_rows() {
        let dir = std::env::temp_dir().join(format!("scribe-chatdb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let _ = std::fs::remove_file(&path);
        let a = Db::open(&path).unwrap();
        let b = Db::open(&path).unwrap();
        let conv = a.current().unwrap();
        let said = a.say(conv, "hello").unwrap();
        b.reply(said, "hi", true).unwrap();
        a.interrupt(said).unwrap();
        let rows = a.since(0).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].role, Role::Assistant);
        assert_eq!(rows[1].reply_to, Some(said));
        assert!(rows[1].more);
        assert_eq!(rows[2].kind, Kind::Interrupt);
        assert_eq!(b.last_id().unwrap(), rows[2].id);
        assert_eq!(a.since(rows[1].id).unwrap().len(), 1);
        assert_eq!(a.setting("voice").unwrap(), None);
        a.set_setting("voice", "af_heart").unwrap();
        b.set_setting("voice", "bm_george").unwrap();
        assert_eq!(a.setting("voice").unwrap().as_deref(), Some("bm_george"));
        // Replies and interrupts land in the user message's conversation.
        assert!(rows.iter().all(|r| r.conversation == conv));
        let other = b.new_conversation().unwrap();
        let said2 = b.say(other, "elsewhere").unwrap();
        a.reply(said2, "there", false).unwrap();
        assert_eq!(a.rows_in(other).unwrap().len(), 2);
        assert_eq!(a.rows_in(conv).unwrap().len(), 3);
        // Most recent first, and the current one is remembered.
        let list = a.conversations().unwrap();
        assert_eq!(
            list.iter().map(|c| c.id).collect::<Vec<_>>(),
            vec![other, conv]
        );
        assert_eq!(list[0].messages, 2);
        a.rename(conv, "Dragons").unwrap();
        assert_eq!(b.title(conv).unwrap().as_deref(), Some("Dragons"));
        a.set_current(conv).unwrap();
        assert_eq!(b.current().unwrap(), conv);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
