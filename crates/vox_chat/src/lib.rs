//! The `scribe --chat` database: one table of turns that scribe and a
//! responder (`agent`, or `scribe chat-echo`) both write. Neither side
//! is notified of the other's writes; each polls for whatever has
//! changed since the revision it last saw.
//!
//! ```sql
//! turns(id, conversation, role, text, rev, done, spoken, at)
//! conversations(id, title, created_at)
//! settings(key, value)
//! ```
//!
//! One row is one turn of the conversation, and it grows: the responder
//! [`Db::start`]s its answer the moment it begins and [`Db::extend`]s it
//! as the words arrive, so scribe can speak the beginning while the end
//! is still being written. Scribe does the same with what it hears,
//! [`Db::revise`]ing the turn as the transcript firms up. `done` says
//! nothing more is coming.
//!
//! Every insert and every update stamps the row with the next `rev`, so
//! one `WHERE rev > ?` (see [`Db::since`]) sees new turns and growing
//! ones alike. That is the only thing either side has to poll.
//!
//! # Where the listener got to
//!
//! Speech is interruptible, so the record has to say how much of a
//! reply reached the ear, not just what was written. `spoken` is that:
//! the number of words of `text` scribe actually played. A turn with
//! `spoken` short of its word count was cut off ([`Turn::cut`]), and
//! [`Turn::heard`] is the part the user got — which is what a responder
//! should put in the model's history, since it is all the user can be
//! answering.
//!
//! Scribe writes `spoken` when it *stops* playing a turn: cut off, or
//! finished. It must not write it merely because it has caught up with
//! a turn still being written, or the turn would read as interrupted.
//! That contract is the one thing the schema can't enforce.
//!
//! # Who is listening
//!
//! A responder says it is there, every [`RESPONDER_BEAT`], with
//! [`Db::responding`]; scribe reads it with [`Db::responder`] and
//! refuses to open a chat nobody is listening to. Nothing else depends
//! on it, so a responder written elsewhere works without one — it just
//! has to be started with `--no-responder` on scribe's side.

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
CREATE TABLE IF NOT EXISTS turns (
    id           INTEGER PRIMARY KEY,
    conversation INTEGER NOT NULL REFERENCES conversations(id),
    role         TEXT    NOT NULL CHECK (role IN ('user', 'assistant')),
    -- Grows as it is written; empty while the responder is still thinking.
    text         TEXT    NOT NULL DEFAULT '',
    -- Bumped past every other row's on each insert and update: what
    -- both sides poll on.
    rev          INTEGER NOT NULL,
    -- Nothing more is coming.
    done         INTEGER NOT NULL DEFAULT 0,
    -- Words of `text` that reached the ear; NULL until it stopped being
    -- spoken. Short of the whole turn means the listener cut it off.
    spoken       INTEGER,
    at           TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX IF NOT EXISTS turns_by_rev ON turns (rev);
CREATE INDEX IF NOT EXISTS turns_by_conversation ON turns (conversation, id);
-- Choices kept with the database: `conversation` (the one last open),
-- `voice.<conversation id>` (the voice of its replies), and the
-- responder's heartbeat: `responder` (what it calls itself) and
-- `responder_at` (unix seconds, by the database's clock).
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

/// The next revision, for the row being written. A single statement, so
/// two writers can't land on the same number: SQLite serializes them.
const NEXT_REV: &str = "(SELECT coalesce(max(rev), 0) + 1 FROM turns)";

/// The columns [`Db::read`] expects, in order.
const COLS: &str = "id, conversation, role, text, rev, done, spoken, at";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

/// One turn of the conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub id: i64,
    pub conversation: i64,
    pub role: Role,
    /// What has been written of it so far.
    pub text: String,
    /// Stamped afresh every time the row is written.
    pub rev: i64,
    /// Nothing more is coming.
    pub done: bool,
    /// Words of `text` that reached the ear, once it stopped being
    /// spoken. `None` while it hasn't been, or is still being, played.
    pub spoken: Option<usize>,
    /// When it was started, UTC: `2026-10-06T17:09:24.123Z`.
    pub at: String,
}

impl Turn {
    /// Words in the whole turn.
    pub fn words(&self) -> usize {
        self.text.split_whitespace().count()
    }

    /// Playback stopped short: the listener only got [`Turn::heard`].
    pub fn cut(&self) -> bool {
        self.spoken.is_some_and(|n| n < self.words())
    }

    /// The part of the turn that reached the ear. The whole of it
    /// unless it was cut off — including while it is still being
    /// spoken, when nothing is known yet and all of it may still be.
    pub fn heard(&self) -> &str {
        let Some(n) = self.spoken else {
            return self.text.trim_end();
        };
        if n == 0 {
            return "";
        }
        // Scanning on from the last word found keeps the cut between
        // words, wherever the whitespace falls.
        let mut end = 0;
        for (seen, w) in self.text.split_whitespace().enumerate() {
            let at = self.text[end..].find(w).map_or(end, |i| end + i);
            end = at + w.len();
            if seen + 1 == n {
                break;
            }
        }
        &self.text[..end]
    }
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

/// Say `who` is listening to the database at `path`, now and every
/// [`RESPONDER_BEAT`] after, on a connection of its own so a long reply
/// doesn't hold the beat up. Returns once the first beat is in, which is
/// what scribe looks for when it opens a chat.
pub fn beat(path: &Path, who: &str) -> Result<()> {
    let db = Db::open(path)?;
    db.responding(who)?;
    let who = who.to_string();
    std::thread::Builder::new()
        .name("beat".into())
        .spawn(move || loop {
            std::thread::sleep(RESPONDER_BEAT);
            if let Err(e) = db.responding(&who) {
                // Nothing to be done about it here; scribe will say
                // the responder has gone quiet.
                eprintln!("saying we are here failed: {e:#}");
            }
        })?;
    Ok(())
}

/// A conversation, for listing.
#[derive(Debug, Clone, PartialEq)]
pub struct Conversation {
    pub id: i64,
    pub title: String,
    /// Its last turn (or its creation), local time, `2026-10-06 17:09`.
    pub last: String,
    pub turns: i64,
}

/// A new conversation's title until it is renamed: the local date and time.
const NOW_TITLE: &str = "strftime('%Y-%m-%d %H:%M', 'now', 'localtime')";

/// How often a responder should say it is still there.
pub const RESPONDER_BEAT: Duration = Duration::from_secs(2);

/// A heartbeat older than this is taken for nobody. Several beats'
/// worth, so a busy machine's late beat doesn't look like a death.
pub const RESPONDER_STALE: Duration = Duration::from_secs(15);

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
                    strftime('%Y-%m-%d %H:%M', coalesce(max(t.at), c.created_at), 'localtime')
                        AS last,
                    count(t.id)
             FROM conversations c LEFT JOIN turns t ON t.conversation = c.id
             GROUP BY c.id
             ORDER BY coalesce(max(t.at), c.created_at) DESC, c.id DESC",
        )?;
        let list = stmt
            .query_map([], |r| {
                Ok(Conversation {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    last: r.get(2)?,
                    turns: r.get(3)?,
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

    /// Begin a turn, with as much of it as is known. Returns its id, to
    /// pass to [`Db::extend`], [`Db::revise`] and [`Db::spoken`].
    pub fn start(&self, conversation: i64, role: Role, text: &str, done: bool) -> Result<i64> {
        self.conn.execute(
            &format!(
                "INSERT INTO turns (conversation, role, text, rev, done)
                 VALUES (?1, ?2, ?3, {NEXT_REV}, ?4)"
            ),
            params![conversation, role.as_str(), text, done as i64],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Add `piece` to the end of a turn, exactly as given: the caller
    /// owns the spacing, so a stream of deltas reads back as it came.
    pub fn extend(&self, turn: i64, piece: &str, done: bool) -> Result<()> {
        self.conn.execute(
            &format!(
                "UPDATE turns SET text = text || ?2, done = ?3, rev = {NEXT_REV} WHERE id = ?1"
            ),
            params![turn, piece, done as i64],
        )?;
        Ok(())
    }

    /// Replace a turn's text: what a transcript does as it firms up.
    pub fn revise(&self, turn: i64, text: &str, done: bool) -> Result<()> {
        self.conn.execute(
            &format!("UPDATE turns SET text = ?2, done = ?3, rev = {NEXT_REV} WHERE id = ?1"),
            params![turn, text, done as i64],
        )?;
        Ok(())
    }

    /// Record that `words` of the turn reached the ear. Only ever
    /// raised: hearing an earlier reply again doesn't unhear it.
    ///
    /// Scribe calls this when it stops playing a turn — never while it
    /// is only waiting for more of one, which would read as a cut.
    pub fn spoken(&self, turn: i64, words: usize) -> Result<()> {
        self.conn.execute(
            &format!(
                "UPDATE turns SET spoken = max(coalesce(spoken, 0), ?2), rev = {NEXT_REV}
                 WHERE id = ?1"
            ),
            params![turn, words as i64],
        )?;
        Ok(())
    }

    /// Everything written or rewritten since revision `rev`, oldest
    /// change first. The one thing both sides poll.
    pub fn since(&self, rev: i64) -> Result<Vec<Turn>> {
        self.read("WHERE rev > ?1 ORDER BY rev", [rev])
    }

    /// Every turn of `conversation`, oldest first.
    pub fn turns_in(&self, conversation: i64) -> Result<Vec<Turn>> {
        self.read("WHERE conversation = ?1 ORDER BY id", [conversation])
    }

    pub fn turn(&self, id: i64) -> Result<Option<Turn>> {
        Ok(self.read("WHERE id = ?1", [id])?.pop())
    }

    /// The newest thing the user said, answered or not. A responder
    /// tracks which one it last took up, rather than reading that off
    /// the order of the rows: a reply begun just as the user said
    /// something new lands *after* that new turn, which would leave it
    /// looking answered for ever.
    pub fn newest_user(&self) -> Result<Option<Turn>> {
        Ok(self
            .read("WHERE role = 'user' ORDER BY id DESC LIMIT 1", [])?
            .pop())
    }

    /// The newest turn, if the user is the one waiting: nothing has
    /// been written after it, so nobody has taken it up. What a
    /// responder checks when it starts, to tell a conversation that
    /// is waiting on it from one that is finished with.
    pub fn unanswered(&self) -> Result<Option<Turn>> {
        Ok(self
            .read(
                "WHERE id = (SELECT max(id) FROM turns) AND role = 'user'",
                [],
            )?
            .pop())
    }

    /// The user has written, or rewritten, something since revision
    /// `rev`: a reply begun before it is answering the wrong thing.
    pub fn said_since(&self, rev: i64) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM turns WHERE role = 'user' AND rev > ?1 LIMIT 1",
                [rev],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// The newest revision in the database; 0 when it is empty.
    pub fn rev(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT max(rev) FROM turns", [], |r| {
                r.get::<_, Option<i64>>(0)
            })
            .optional()?
            .flatten()
            .unwrap_or(0))
    }

    fn read(&self, filter: &str, args: impl rusqlite::Params) -> Result<Vec<Turn>> {
        let mut stmt = self
            .conn
            .prepare_cached(&format!("SELECT {COLS} FROM turns {filter}"))?;
        let turns = stmt
            .query_map(args, |r| {
                Ok(Turn {
                    id: r.get(0)?,
                    conversation: r.get(1)?,
                    role: if r.get::<_, String>(2)? == "assistant" {
                        Role::Assistant
                    } else {
                        Role::User
                    },
                    text: r.get(3)?,
                    rev: r.get(4)?,
                    done: r.get::<_, i64>(5)? != 0,
                    spoken: r.get::<_, Option<i64>>(6)?.map(|n| n as usize),
                    at: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(turns)
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

    /// Say that `who` is listening to this database, now — every
    /// [`RESPONDER_BEAT`] for as long as it runs. The time is the
    /// database's own, so the two sides needn't agree on the clock.
    pub fn responding(&self, who: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value)
             VALUES ('responder', ?1), ('responder_at', strftime('%s', 'now'))
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![who],
        )?;
        Ok(())
    }

    /// Who is answering this database, if their heartbeat is younger
    /// than `stale`; `None` when nobody is, or nobody lately.
    pub fn responder(&self, stale: Duration) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT (SELECT value FROM settings WHERE key = 'responder')
                 WHERE CAST((SELECT value FROM settings WHERE key = 'responder_at')
                           AS INTEGER) > CAST(strftime('%s', 'now') AS INTEGER) - ?1",
                [stale.as_secs() as i64],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A database of its own per test, so they can run at once.
    fn db(name: &str) -> (PathBuf, Db) {
        let dir = std::env::temp_dir().join(format!("vox-chat-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let _ = std::fs::remove_file(&path);
        let open = Db::open(&path).unwrap();
        (path, open)
    }

    #[test]
    fn names_get_db() {
        assert_eq!(path_for(Path::new("talk")), PathBuf::from("talk.db"));
        assert_eq!(path_for(Path::new("talk.db")), PathBuf::from("talk.db"));
        assert_eq!(path_for(Path::new("a.b")), PathBuf::from("a.b.db"));
    }

    #[test]
    fn a_responder_is_only_there_while_it_beats() {
        let (_p, db) = db("responder");
        // Nobody has said anything yet.
        assert_eq!(db.responder(RESPONDER_STALE).unwrap(), None);
        db.responding("agent 123").unwrap();
        assert_eq!(
            db.responder(RESPONDER_STALE).unwrap().as_deref(),
            Some("agent 123")
        );
        // Beats stop: a moment later nobody is answering.
        assert_eq!(db.responder(Duration::ZERO).unwrap(), None);
        // And a new responder takes over by beating itself.
        db.responding("chat-echo 456").unwrap();
        assert_eq!(
            db.responder(RESPONDER_STALE).unwrap().as_deref(),
            Some("chat-echo 456")
        );
    }

    /// The whole point of `rev`: one poll sees a turn appear and then
    /// sees it grow, without the reader having to ask about it again.
    #[test]
    fn one_poll_sees_new_turns_and_growing_ones() {
        let (path, a) = db("rev");
        let b = Db::open(&path).unwrap();
        let conv = a.current().unwrap();
        let mut seen = b.rev().unwrap();
        assert_eq!(seen, 0);

        let said = a.start(conv, Role::User, "Tell me a", false).unwrap();
        let new = b.since(seen).unwrap();
        assert_eq!(new.len(), 1);
        assert!(!new[0].done);
        seen = new[0].rev;

        // The transcript firms up: the same row, a later revision.
        a.revise(said, "Tell me a joke.", true).unwrap();
        let new = b.since(seen).unwrap();
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].id, said);
        assert_eq!(new[0].text, "Tell me a joke.");
        assert!(new[0].done);
        seen = new[0].rev;
        assert!(seen > 1);

        // A reply begins empty — that is what "thinking" looks like —
        // and is appended to verbatim, spacing and all.
        let reply = b.start(conv, Role::Assistant, "", false).unwrap();
        b.extend(reply, "Why did the", false).unwrap();
        b.extend(reply, " chicken cross the road?", true).unwrap();
        let turn = a.turn(reply).unwrap().unwrap();
        assert_eq!(turn.text, "Why did the chicken cross the road?");
        assert!(turn.done);
        // Three writes to one row, but only the one row to catch up on.
        assert_eq!(a.since(seen).unwrap().len(), 1);
        assert!(turn.rev > seen);
    }

    #[test]
    fn a_turn_knows_how_much_of_it_was_heard() {
        let (_p, db) = db("heard");
        let conv = db.current().unwrap();
        let id = db
            .start(conv, Role::Assistant, "One two three four five.", true)
            .unwrap();

        // Nothing is known until playback stops: all of it may yet be.
        let t = db.turn(id).unwrap().unwrap();
        assert_eq!(t.spoken, None);
        assert!(!t.cut());
        assert_eq!(t.heard(), "One two three four five.");

        db.spoken(id, 2).unwrap();
        let t = db.turn(id).unwrap().unwrap();
        assert!(t.cut());
        assert_eq!(t.heard(), "One two");

        // Heard again, further in: it only ever rises.
        db.spoken(id, 4).unwrap();
        assert_eq!(db.turn(id).unwrap().unwrap().heard(), "One two three four");
        db.spoken(id, 1).unwrap();
        assert_eq!(db.turn(id).unwrap().unwrap().heard(), "One two three four");

        // Spoken to the end: not a cut.
        db.spoken(id, 5).unwrap();
        let t = db.turn(id).unwrap().unwrap();
        assert!(!t.cut());
        assert_eq!(t.heard(), "One two three four five.");
    }

    #[test]
    fn nothing_heard_is_not_the_whole_turn() {
        let (_p, db) = db("unheard");
        let conv = db.current().unwrap();
        let id = db.start(conv, Role::Assistant, "Never mind.", false).unwrap();
        db.spoken(id, 0).unwrap();
        let t = db.turn(id).unwrap().unwrap();
        assert_eq!(t.heard(), "");
        assert!(t.cut());
    }

    /// A responder's two questions: is there anything to answer, and is
    /// the answer I am writing still the one that was wanted?
    #[test]
    fn a_responder_knows_what_to_answer_and_when_to_stop() {
        let (_p, db) = db("answering");
        let conv = db.current().unwrap();
        assert_eq!(db.unanswered().unwrap(), None);

        let said = db.start(conv, Role::User, "Hello.", true).unwrap();
        let waiting = db.unanswered().unwrap().unwrap();
        assert_eq!(waiting.id, said);
        assert!(waiting.done);

        // Taking it up is enough to stop it being unanswered.
        let reply = db.start(conv, Role::Assistant, "", false).unwrap();
        assert_eq!(db.unanswered().unwrap(), None);
        assert!(!db.said_since(waiting.rev).unwrap());

        // The user talks over it: the reply in progress is stale.
        db.start(conv, Role::User, "Actually, no.", true).unwrap();
        assert!(db.said_since(waiting.rev).unwrap());
        db.extend(reply, "Hi there.", true).unwrap();
        db.spoken(reply, 1).unwrap();
        // ...and the newest thing said is waiting for an answer.
        assert_eq!(
            db.unanswered().unwrap().map(|t| t.text),
            Some("Actually, no.".into())
        );
    }

    /// Revising a message counts as saying something: a reply begun
    /// against the half-heard version of it is answering the wrong text.
    #[test]
    fn rewriting_what_was_said_supersedes_a_reply_too() {
        let (_p, db) = db("revised");
        let conv = db.current().unwrap();
        let said = db.start(conv, Role::User, "Play the", false).unwrap();
        let at = db.turn(said).unwrap().unwrap().rev;
        db.start(conv, Role::Assistant, "", false).unwrap();
        assert!(!db.said_since(at).unwrap());
        db.revise(said, "Play the second one.", true).unwrap();
        assert!(db.said_since(at).unwrap());
    }

    /// A reply begun just as the user says something new is written
    /// after it, so the row order alone makes the new turn look
    /// answered. That is why a responder tracks what it took up.
    #[test]
    fn a_late_reply_hides_the_turn_it_did_not_answer() {
        let (_p, db) = db("late");
        let conv = db.current().unwrap();
        let first = db.start(conv, Role::User, "Hello.", true).unwrap();
        // The user carries on before the responder has written a word.
        let second = db.start(conv, Role::User, "Actually, wait.", true).unwrap();
        // ...and only now does the reply to the *first* turn appear.
        db.start(conv, Role::Assistant, "", false).unwrap();

        // Nothing is the newest row, so nothing looks unanswered —
        // though the second turn has had no answer at all.
        assert_eq!(db.unanswered().unwrap(), None);
        let newest = db.newest_user().unwrap().unwrap();
        assert_eq!(newest.id, second);
        assert_ne!(newest.id, first);
    }

    #[test]
    fn conversations_keep_their_own_turns() {
        let (path, a) = db("conversations");
        let b = Db::open(&path).unwrap();
        let conv = a.current().unwrap();
        a.start(conv, Role::User, "Hi.", true).unwrap();
        a.start(conv, Role::Assistant, "Hello.", true).unwrap();
        let other = b.new_conversation().unwrap();
        b.start(other, Role::User, "Elsewhere.", true).unwrap();
        assert_eq!(a.turns_in(conv).unwrap().len(), 2);
        assert_eq!(a.turns_in(other).unwrap().len(), 1);

        // Most recent first, with its turns counted.
        let list = a.conversations().unwrap();
        assert_eq!(
            list.iter().map(|c| c.id).collect::<Vec<_>>(),
            vec![other, conv]
        );
        assert_eq!(list[0].turns, 1);

        a.rename(conv, "Dragons").unwrap();
        assert_eq!(b.title(conv).unwrap().as_deref(), Some("Dragons"));
        a.set_current(conv).unwrap();
        assert_eq!(b.current().unwrap(), conv);

        // Settings are shared by whoever has the file open.
        assert_eq!(a.setting("voice").unwrap(), None);
        a.set_setting("voice", "af_heart").unwrap();
        b.set_setting("voice", "bm_george").unwrap();
        assert_eq!(a.setting("voice").unwrap().as_deref(), Some("bm_george"));
    }
}
