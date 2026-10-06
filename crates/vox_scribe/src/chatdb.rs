//! The `--chat` database: one SQLite table that scribe and a responder
//! (an LLM bridge, or `scribe chat-echo`) both write. Neither side is
//! notified of the other's writes; each polls for rows after the last
//! id it has seen, which is cheap and well under the time a reply takes.
//!
//! ```sql
//! messages(id, role, kind, reply_to, text, more, at)
//! ```
//!
//! * `role = 'user', kind = 'say'`: something the user said and sent
//!   with Enter.
//! * `role = 'assistant', kind = 'say'`: part of the reply to user row
//!   `reply_to`, spoken in id order. A reply may come in any number of
//!   rows (a sentence at a time, or several separate utterances); every
//!   row but the last has `more = 1`, and scribe keeps waiting for the
//!   rest. The last has `more = 0`, and scribe listens again once it has
//!   been spoken. A row may have empty text, e.g. `more = 1` alone to
//!   say "still working on it".
//! * `role = 'user', kind = 'interrupt'`: the user cut off the reply to
//!   `reply_to` (by talking over it). The responder should stop writing
//!   that reply; scribe ignores any more rows for it. A new `say` from
//!   the user also supersedes any reply still in progress.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result};
use rusqlite::{params, Connection, OptionalExtension};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS messages (
    id       INTEGER PRIMARY KEY,
    role     TEXT    NOT NULL CHECK (role IN ('user', 'assistant')),
    kind     TEXT    NOT NULL DEFAULT 'say' CHECK (kind IN ('say', 'interrupt')),
    reply_to INTEGER REFERENCES messages(id),
    text     TEXT    NOT NULL DEFAULT '',
    more     INTEGER NOT NULL DEFAULT 0,
    at       TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
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

    /// The user said `text`. Returns its id.
    pub fn say(&self, text: &str) -> Result<i64> {
        self.insert(Role::User, Kind::Say, None, text, false)
    }

    /// The user cut off the reply to `reply_to`.
    pub fn interrupt(&self, reply_to: i64) -> Result<i64> {
        self.insert(Role::User, Kind::Interrupt, Some(reply_to), "", false)
    }

    /// Part of the reply to `reply_to`; `more` if the rest is coming.
    pub fn reply(&self, reply_to: i64, text: &str, more: bool) -> Result<i64> {
        self.insert(Role::Assistant, Kind::Say, Some(reply_to), text, more)
    }

    fn insert(
        &self,
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
            "INSERT INTO messages (role, kind, reply_to, text, more) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![role, kind, reply_to, text, more as i64],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Rows after `id`, oldest first.
    pub fn since(&self, id: i64) -> Result<Vec<Row>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, role, kind, reply_to, text, more, at FROM messages WHERE id > ?1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map([id], |r| {
                Ok(Row {
                    id: r.get(0)?,
                    role: if r.get::<_, String>(1)? == "assistant" {
                        Role::Assistant
                    } else {
                        Role::User
                    },
                    kind: if r.get::<_, String>(2)? == "interrupt" {
                        Kind::Interrupt
                    } else {
                        Kind::Say
                    },
                    reply_to: r.get(3)?,
                    text: r.get(4)?,
                    more: r.get::<_, i64>(5)? != 0,
                    at: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
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

/// `scribe chat-echo NAME`: a stand-in responder. Each thing the user
/// says is read back a sentence per row, then, after a pause during
/// which scribe waits (`more`), a second utterance. Stops a reply as
/// soon as the user interrupts it or says something new.
pub fn echo(path: &Path) -> Result<()> {
    let db = Db::open(path)?;
    let mut last = db.last_id()?;
    println!("echoing replies in {} (ctrl+c to stop)", path.display());
    loop {
        let rows = db.since(last)?;
        let Some(say) = rows
            .iter()
            .rev()
            .find(|r| r.role == Role::User && r.kind == Kind::Say)
            .cloned()
        else {
            if let Some(r) = rows.last() {
                last = r.id;
            }
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        // Only the newest thing said gets a reply.
        last = say.id;
        println!("user #{}: {}", say.id, say.text);
        let words = say.text.split_whitespace().count();
        let mut parts: Vec<(String, u64)> =
            crate::speak::sentences(&format!("You said: {}", say.text))
                .into_iter()
                .map(|s| (s, 300))
                .collect();
        // A second utterance after a pause, to show `more`.
        parts.push((
            format!(
                "That was {words} word{}. What else?",
                if words == 1 { "" } else { "s" }
            ),
            1500,
        ));
        let n = parts.len();
        for (i, (text, delay)) in parts.into_iter().enumerate() {
            std::thread::sleep(Duration::from_millis(delay));
            let cut = db.since(say.id)?.into_iter().any(|r| r.role == Role::User);
            if cut {
                println!("  interrupted");
                break;
            }
            db.reply(say.id, &text, i + 1 < n)?;
            println!("  reply: {text}");
        }
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
        let said = a.say("hello").unwrap();
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
        let _ = std::fs::remove_dir_all(&dir);
    }
}
