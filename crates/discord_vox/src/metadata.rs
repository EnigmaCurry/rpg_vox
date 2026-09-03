//! Metadata store for Discord identifiers seen by the bot.
//!
//! SQLite-backed. Populated opportunistically as the bot joins guilds and
//! observes speakers. Read side is deliberately absent for now — a later
//! API will consume this data. Snowflakes are u64 in the Discord API and
//! stored as INTEGER (SQLite's native i64) here; callers cast.
//!
//! All writes are best-effort: DB errors are logged but do not fail the
//! caller. Missing rows / stale rows are expected.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context as _, Result};
use rusqlite::{Connection, params};
use tracing::warn;

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS guilds (
        id          INTEGER PRIMARY KEY,
        name        TEXT NOT NULL,
        updated_at  INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS channels (
        id          INTEGER PRIMARY KEY,
        guild_id    INTEGER NOT NULL,
        name        TEXT NOT NULL,
        updated_at  INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_channels_guild ON channels(guild_id);

    CREATE TABLE IF NOT EXISTS users (
        id           INTEGER PRIMARY KEY,
        username     TEXT NOT NULL,
        global_name  TEXT,
        best_name    TEXT NOT NULL,
        updated_at   INTEGER NOT NULL
    );

    -- Per-guild nickname is separate from the global user identity.
    CREATE TABLE IF NOT EXISTS guild_members (
        guild_id    INTEGER NOT NULL,
        user_id     INTEGER NOT NULL,
        nick        TEXT,
        updated_at  INTEGER NOT NULL,
        PRIMARY KEY (guild_id, user_id)
    );
";

pub struct MetadataDb {
    conn: Mutex<Connection>,
}

impl MetadataDb {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create metadata db parent {}", parent.display()))?;
            }
        }
        let conn = Connection::open(path)
            .with_context(|| format!("open metadata db at {}", path.display()))?;
        // Set some sane pragmas: WAL for concurrent read while writing, and
        // NORMAL synchronous (crash-safe enough for metadata).
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; \
             PRAGMA synchronous = NORMAL; \
             PRAGMA foreign_keys = ON;",
        )
        .context("apply metadata db pragmas")?;
        conn.execute_batch(SCHEMA)
            .context("apply metadata schema")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn upsert_guild(&self, id: u64, name: &str) {
        let now = chrono::Utc::now().timestamp();
        let conn = self.conn.lock().unwrap();
        if let Err(err) = conn.execute(
            "INSERT INTO guilds (id, name, updated_at) VALUES (?1, ?2, ?3) \
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, \
                                           updated_at = excluded.updated_at",
            params![id as i64, name, now],
        ) {
            warn!(?err, id, "upsert_guild failed");
        }
    }

    pub fn upsert_channel(&self, id: u64, guild_id: u64, name: &str) {
        let now = chrono::Utc::now().timestamp();
        let conn = self.conn.lock().unwrap();
        if let Err(err) = conn.execute(
            "INSERT INTO channels (id, guild_id, name, updated_at) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(id) DO UPDATE SET guild_id = excluded.guild_id, \
                                           name = excluded.name, \
                                           updated_at = excluded.updated_at",
            params![id as i64, guild_id as i64, name, now],
        ) {
            warn!(?err, id, "upsert_channel failed");
        }
    }

    /// `best_name` is whichever display name the caller prefers (typically
    /// guild-nick → global-name → username), stored so downstream tools
    /// don't have to reimplement that fallback chain.
    pub fn upsert_user(&self, id: u64, username: &str, global_name: Option<&str>, best_name: &str) {
        let now = chrono::Utc::now().timestamp();
        let conn = self.conn.lock().unwrap();
        if let Err(err) = conn.execute(
            "INSERT INTO users (id, username, global_name, best_name, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(id) DO UPDATE SET username = excluded.username, \
                                           global_name = excluded.global_name, \
                                           best_name = excluded.best_name, \
                                           updated_at = excluded.updated_at",
            params![id as i64, username, global_name, best_name, now],
        ) {
            warn!(?err, id, "upsert_user failed");
        }
    }

    pub fn upsert_guild_member(&self, guild_id: u64, user_id: u64, nick: Option<&str>) {
        let now = chrono::Utc::now().timestamp();
        let conn = self.conn.lock().unwrap();
        if let Err(err) = conn.execute(
            "INSERT INTO guild_members (guild_id, user_id, nick, updated_at) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(guild_id, user_id) DO UPDATE SET nick = excluded.nick, \
                                                          updated_at = excluded.updated_at",
            params![guild_id as i64, user_id as i64, nick, now],
        ) {
            warn!(?err, guild_id, user_id, "upsert_guild_member failed");
        }
    }
}
