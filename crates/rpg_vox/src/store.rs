//! Persistent store for designed speech clips.
//!
//! Two backing pieces, both under a single `--data-dir` (default `./data`):
//!
//! * `data/rpg_vox.sqlite` — one row per widget (id, text, instruct, clip
//!   metadata) and one row per image (id, mime). Accessed through rusqlite
//!   behind a Mutex; every call is wrapped in `spawn_blocking` so the tokio
//!   runtime doesn't stall on sqlite I/O.
//! * `data/clips/{id}.wav` — the rendered PCM as WAV, one file per widget.
//!   Streamed to the browser on render and deleted alongside the DB row
//!   on delete.
//! * `data/images/{id}` — character avatars + reference pictures uploaded
//!   from the browser. Extension-less because the mime type lives on the
//!   sqlite row and browsers rely on Content-Type, not the URL suffix.
//!
//! No caching layer over sqlite — the widget count in this app is small and
//! every access is user-triggered, so the extra complexity isn't worth it.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::mixer::MixerState;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS widgets (
    id           TEXT PRIMARY KEY,
    text         TEXT NOT NULL,
    instruct     TEXT,
    sample_rate  INTEGER NOT NULL,
    duration_ms  INTEGER NOT NULL,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS images (
    id           TEXT PRIMARY KEY,
    mime         TEXT NOT NULL,
    byte_size    INTEGER NOT NULL,
    created_at   INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS app_state (
    key          TEXT PRIMARY KEY,
    value        TEXT NOT NULL,
    updated_at   INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS scripts (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS script_turns (
    id           TEXT PRIMARY KEY,
    script_id    TEXT NOT NULL REFERENCES scripts(id) ON DELETE CASCADE,
    ord          INTEGER NOT NULL,
    role         TEXT NOT NULL,
    content      TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    UNIQUE(script_id, ord)
);
CREATE TABLE IF NOT EXISTS script_speech_blocks (
    id            TEXT PRIMARY KEY,
    turn_id       TEXT NOT NULL REFERENCES script_turns(id) ON DELETE CASCADE,
    ord           INTEGER NOT NULL,
    text          TEXT NOT NULL,
    selected_take INTEGER,
    UNIQUE(turn_id, ord)
);
CREATE TABLE IF NOT EXISTS script_speech_takes (
    id           TEXT PRIMARY KEY,
    block_id     TEXT NOT NULL REFERENCES script_speech_blocks(id) ON DELETE CASCADE,
    ord          INTEGER NOT NULL,
    widget_id    TEXT NOT NULL REFERENCES widgets(id) ON DELETE CASCADE,
    created_at   INTEGER NOT NULL,
    UNIQUE(block_id, ord)
);
"#;

/// Single hardcoded script id — MVP has one shared conversation, matching
/// the legacy /chat semantics. Multi-script (per-project) comes later.
pub const DEFAULT_SCRIPT_ID: &str = "default";

/// Single row key for the whole projects/characters/scenes blob. Keeping the
/// table generic-KV lets us stash other server-side prefs here later without
/// migrating.
const APP_STATE_KEY: &str = "app";

/// Row key for the persisted mixer JSON. Same KV table as `APP_STATE_KEY`
/// so the mixer piggybacks on an already-migrated store.
const MIXER_STATE_KEY: &str = "mixer";

#[derive(Clone)]
pub struct Store {
    db: Arc<Mutex<Connection>>,
    clips_dir: PathBuf,
    images_dir: PathBuf,
}

impl Store {
    /// Open (or create) the store rooted at `data_dir`. Creates the directory
    /// structure and applies the schema idempotently.
    pub fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("creating data dir {}", data_dir.display()))?;
        let clips_dir = data_dir.join("clips");
        std::fs::create_dir_all(&clips_dir)
            .with_context(|| format!("creating clips dir {}", clips_dir.display()))?;
        let images_dir = data_dir.join("images");
        std::fs::create_dir_all(&images_dir)
            .with_context(|| format!("creating images dir {}", images_dir.display()))?;

        let db_path = data_dir.join("rpg_vox.sqlite");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("opening sqlite at {}", db_path.display()))?;
        // ON DELETE CASCADE only fires when foreign_keys pragma is on; sqlite
        // defaults it off per-connection. Set before the schema applies so any
        // downstream migration can rely on FK checks too.
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .context("enabling foreign_keys pragma")?;
        conn.execute_batch(SCHEMA).context("applying schema")?;

        Ok(Self {
            db: Arc::new(Mutex::new(conn)),
            clips_dir,
            images_dir,
        })
    }

    pub fn clip_path(&self, id: &str) -> PathBuf {
        self.clips_dir.join(format!("{id}.wav"))
    }

    pub fn image_path(&self, id: &str) -> PathBuf {
        self.images_dir.join(id)
    }

    /// Look up an existing widget's clip metadata. Returns `Ok(None)` if the
    /// row doesn't exist so callers can distinguish "missing" from an error.
    pub async fn get_widget(&self, id: String) -> Result<Option<WidgetRow>> {
        let db = self.db.clone();
        let id_clone = id.clone();
        let row = tokio::task::spawn_blocking(move || -> Result<Option<WidgetRow>> {
            let conn = db.lock().unwrap();
            let row = conn
                .query_row(
                    "SELECT sample_rate, duration_ms FROM widgets WHERE id = ?1",
                    params![id_clone],
                    |r| {
                        Ok(WidgetRow {
                            sample_rate: r.get::<_, i64>(0)? as u32,
                            duration_ms: r.get::<_, i64>(1)? as u64,
                        })
                    },
                )
                .optional()?;
            Ok(row)
        })
        .await
        .context("db task panicked")??;
        Ok(row)
    }

    /// Create a new widget row and write its WAV. Returns the freshly
    /// assigned id. Wraps sqlite work in `spawn_blocking`; file write goes
    /// through `tokio::fs` after the row is committed.
    pub async fn create_widget(
        &self,
        text: String,
        instruct: Option<String>,
        sample_rate: u32,
        duration_ms: u64,
        wav_bytes: Vec<u8>,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = unix_now();
        let db = self.db.clone();
        let id_clone = id.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO widgets
                   (id, text, instruct, sample_rate, duration_ms, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![id_clone, text, instruct, sample_rate, duration_ms as i64, now, now],
            )?;
            Ok(())
        })
        .await
        .context("db task panicked")??;

        tokio::fs::write(self.clip_path(&id), wav_bytes)
            .await
            .with_context(|| format!("writing clip for {id}"))?;

        Ok(id)
    }

    /// Update an existing widget row and overwrite its WAV. Errors if the id
    /// doesn't exist (so the client gets a 404 rather than silently creating
    /// a new record under an ID the server never issued).
    pub async fn update_widget(
        &self,
        id: String,
        text: String,
        instruct: Option<String>,
        sample_rate: u32,
        duration_ms: u64,
        wav_bytes: Vec<u8>,
    ) -> Result<UpdateResult> {
        let now = unix_now();
        let db = self.db.clone();
        let id_clone = id.clone();
        let rows = tokio::task::spawn_blocking(move || -> Result<usize> {
            let conn = db.lock().unwrap();
            let rows = conn.execute(
                "UPDATE widgets
                    SET text = ?2, instruct = ?3, sample_rate = ?4,
                        duration_ms = ?5, updated_at = ?6
                  WHERE id = ?1",
                params![id_clone, text, instruct, sample_rate, duration_ms as i64, now],
            )?;
            Ok(rows)
        })
        .await
        .context("db task panicked")??;

        if rows == 0 {
            return Ok(UpdateResult::NotFound);
        }

        tokio::fs::write(self.clip_path(&id), wav_bytes)
            .await
            .with_context(|| format!("writing clip for {id}"))?;

        Ok(UpdateResult::Updated)
    }

    /// Update just the widget's `text` column, leaving the WAV and other
    /// metadata (sample_rate, duration_ms, instruct) untouched. Used by the
    /// "save" action in the editing form so the user can correct an STT
    /// transcript without re-synthesizing over a recorded clip.
    pub async fn update_widget_text(
        &self,
        id: String,
        text: String,
    ) -> Result<UpdateResult> {
        let now = unix_now();
        let db = self.db.clone();
        let rows = tokio::task::spawn_blocking(move || -> Result<usize> {
            let conn = db.lock().unwrap();
            let rows = conn.execute(
                "UPDATE widgets SET text = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, text, now],
            )?;
            Ok(rows)
        })
        .await
        .context("db task panicked")??;
        if rows == 0 {
            return Ok(UpdateResult::NotFound);
        }
        Ok(UpdateResult::Updated)
    }

    /// Delete row + WAV. Idempotent: unknown id is treated as success (the
    /// caller's goal is "make sure this widget is gone"). Missing WAV file
    /// is likewise fine — the DB row is authoritative and we log a warning
    /// but still return Ok.
    pub async fn delete_widget(&self, id: String) -> Result<()> {
        let db = self.db.clone();
        let id_clone = id.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = db.lock().unwrap();
            conn.execute("DELETE FROM widgets WHERE id = ?1", params![id_clone])?;
            Ok(())
        })
        .await
        .context("db task panicked")??;

        let path = self.clip_path(&id);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!(id, "clip file was already missing on delete");
            }
            Err(e) => {
                return Err(e).with_context(|| format!("removing clip for {id}"));
            }
        }
        Ok(())
    }

    /// Insert a new image row and write its file. Returns the freshly
    /// assigned id. `mime` is stored so `get_image` can echo it back as the
    /// Content-Type header without sniffing bytes on every request.
    pub async fn create_image(&self, mime: String, bytes: Vec<u8>) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = unix_now();
        let byte_size = bytes.len() as i64;
        let db = self.db.clone();
        let id_clone = id.clone();
        let mime_clone = mime.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO images (id, mime, byte_size, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![id_clone, mime_clone, byte_size, now],
            )?;
            Ok(())
        })
        .await
        .context("db task panicked")??;

        tokio::fs::write(self.image_path(&id), bytes)
            .await
            .with_context(|| format!("writing image for {id}"))?;

        Ok(id)
    }

    /// Look up an image's mime + size. Returns `Ok(None)` for missing rows
    /// so callers can 404 without an error branch.
    pub async fn get_image(&self, id: String) -> Result<Option<ImageRow>> {
        let db = self.db.clone();
        let id_clone = id.clone();
        let row = tokio::task::spawn_blocking(move || -> Result<Option<ImageRow>> {
            let conn = db.lock().unwrap();
            let row = conn
                .query_row(
                    "SELECT mime, byte_size FROM images WHERE id = ?1",
                    params![id_clone],
                    |r| {
                        Ok(ImageRow {
                            mime: r.get::<_, String>(0)?,
                            byte_size: r.get::<_, i64>(1)? as u64,
                        })
                    },
                )
                .optional()?;
            Ok(row)
        })
        .await
        .context("db task panicked")??;
        Ok(row)
    }

    /// Idempotent: unknown id (and missing on-disk file) both return Ok(()).
    /// Matches `delete_widget` so cascade-delete on the client can fire and
    /// forget without racing on already-gone ids.
    pub async fn delete_image(&self, id: String) -> Result<()> {
        let db = self.db.clone();
        let id_clone = id.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = db.lock().unwrap();
            conn.execute("DELETE FROM images WHERE id = ?1", params![id_clone])?;
            Ok(())
        })
        .await
        .context("db task panicked")??;

        let path = self.image_path(&id);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!(id, "image file was already missing on delete");
            }
            Err(e) => {
                return Err(e).with_context(|| format!("removing image for {id}"));
            }
        }
        Ok(())
    }

    /// Read the single app_state JSON blob. Returns Ok(None) if the row
    /// hasn't been written yet (fresh install) so the caller can decide
    /// whether to fall back to a legacy source (e.g. localStorage migration).
    pub async fn get_app_state(&self) -> Result<Option<String>> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<String>> {
            let conn = db.lock().unwrap();
            let value = conn
                .query_row(
                    "SELECT value FROM app_state WHERE key = ?1",
                    params![APP_STATE_KEY],
                    |r| r.get::<_, String>(0),
                )
                .optional()?;
            Ok(value)
        })
        .await
        .context("db task panicked")?
    }

    /// Upsert the app_state JSON blob. The caller has already validated that
    /// the payload is well-formed JSON; the store treats it as opaque text.
    pub async fn put_app_state(&self, value: String) -> Result<()> {
        let now = unix_now();
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO app_state (key, value, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value,
                                                updated_at = excluded.updated_at",
                params![APP_STATE_KEY, value, now],
            )?;
            Ok(())
        })
        .await
        .context("db task panicked")??;
        Ok(())
    }

    /// Read the persisted mixer state. Returns Ok(None) on a fresh install
    /// so the caller can fall back to defaults. Malformed JSON is treated
    /// the same as missing — we log a warning and return None rather than
    /// booting into a hard error over a settings blob.
    pub async fn get_mixer(&self) -> Result<Option<MixerState>> {
        let db = self.db.clone();
        let raw = tokio::task::spawn_blocking(move || -> Result<Option<String>> {
            let conn = db.lock().unwrap();
            let value = conn
                .query_row(
                    "SELECT value FROM app_state WHERE key = ?1",
                    params![MIXER_STATE_KEY],
                    |r| r.get::<_, String>(0),
                )
                .optional()?;
            Ok(value)
        })
        .await
        .context("db task panicked")??;
        Ok(raw.and_then(|text| match serde_json::from_str::<MixerState>(&text) {
            Ok(s) => Some(s),
            Err(err) => {
                tracing::warn!(err = %err, "persisted mixer state didn't parse; using defaults");
                None
            }
        }))
    }

    /// Upsert the mixer state row. The mixer JSON is small (few dozen
    /// bytes) so we don't bother with any staleness checks.
    pub async fn put_mixer(&self, state: MixerState) -> Result<()> {
        let value = serde_json::to_string(&state).context("serialize mixer state")?;
        let now = unix_now();
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO app_state (key, value, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value,
                                                updated_at = excluded.updated_at",
                params![MIXER_STATE_KEY, value, now],
            )?;
            Ok(())
        })
        .await
        .context("db task panicked")??;
        Ok(())
    }

    /// Insert the default script row if it doesn't already exist. Called at
    /// boot so `/script` endpoints can assume the row is there without a
    /// nested "create if missing" branch on every write.
    pub async fn ensure_default_script(&self) -> Result<()> {
        let now = unix_now();
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT OR IGNORE INTO scripts (id, name, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![DEFAULT_SCRIPT_ID, "default", now, now],
            )?;
            Ok(())
        })
        .await
        .context("db task panicked")??;
        Ok(())
    }

    /// Read the full script (turns + blocks + takes) in ordering-stable form.
    /// Empty script returns Ok with an empty turns list.
    pub async fn get_script(&self, script_id: String) -> Result<ScriptRow> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || -> Result<ScriptRow> {
            let conn = db.lock().unwrap();
            let mut turns_stmt = conn.prepare(
                "SELECT id, ord, role, content FROM script_turns
                 WHERE script_id = ?1 ORDER BY ord ASC",
            )?;
            let turns_iter = turns_stmt.query_map(params![script_id], |r| {
                Ok(ScriptTurnRow {
                    id: r.get::<_, String>(0)?,
                    ord: r.get::<_, i64>(1)?,
                    role: r.get::<_, String>(2)?,
                    content: r.get::<_, String>(3)?,
                    blocks: Vec::new(),
                })
            })?;
            let mut turns: Vec<ScriptTurnRow> =
                turns_iter.collect::<rusqlite::Result<Vec<_>>>()?;

            for turn in turns.iter_mut() {
                let mut block_stmt = conn.prepare(
                    "SELECT id, ord, text, selected_take
                     FROM script_speech_blocks
                     WHERE turn_id = ?1 ORDER BY ord ASC",
                )?;
                let blocks_iter = block_stmt.query_map(params![turn.id], |r| {
                    Ok(ScriptBlockRow {
                        id: r.get::<_, String>(0)?,
                        ord: r.get::<_, i64>(1)?,
                        text: r.get::<_, String>(2)?,
                        selected_take: r.get::<_, Option<i64>>(3)?,
                        takes: Vec::new(),
                    })
                })?;
                let mut blocks: Vec<ScriptBlockRow> =
                    blocks_iter.collect::<rusqlite::Result<Vec<_>>>()?;
                for block in blocks.iter_mut() {
                    let mut take_stmt = conn.prepare(
                        "SELECT id, ord, widget_id FROM script_speech_takes
                         WHERE block_id = ?1 ORDER BY ord ASC",
                    )?;
                    let takes_iter = take_stmt.query_map(params![block.id], |r| {
                        Ok(ScriptTakeRow {
                            id: r.get::<_, String>(0)?,
                            ord: r.get::<_, i64>(1)?,
                            widget_id: r.get::<_, String>(2)?,
                        })
                    })?;
                    block.takes = takes_iter.collect::<rusqlite::Result<Vec<_>>>()?;
                }
                turn.blocks = blocks;
            }

            Ok(ScriptRow {
                id: script_id,
                turns,
            })
        })
        .await
        .context("db task panicked")?
    }

    /// Append a turn (with any speech blocks parsed from its content) to a
    /// script and return the freshly-created turn row (blocks included, no
    /// takes yet). Called for both user turns (no blocks) and assistant
    /// turns (blocks pre-populated from `<speak>` parsing).
    pub async fn create_turn(
        &self,
        script_id: String,
        role: String,
        content: String,
        blocks: Vec<String>,
    ) -> Result<ScriptTurnRow> {
        let turn_id = Uuid::new_v4().to_string();
        let block_ids: Vec<String> = blocks.iter().map(|_| Uuid::new_v4().to_string()).collect();
        let now = unix_now();
        let db = self.db.clone();
        let turn_id_clone = turn_id.clone();
        let block_ids_clone = block_ids.clone();
        let blocks_clone = blocks.clone();
        let content_clone = content.clone();
        let role_clone = role.clone();
        let script_id_clone = script_id.clone();
        let (ord, block_rows) = tokio::task::spawn_blocking(
            move || -> Result<(i64, Vec<ScriptBlockRow>)> {
                let mut conn = db.lock().unwrap();
                let tx = conn.transaction()?;
                let ord: i64 = tx
                    .query_row(
                        "SELECT COALESCE(MAX(ord), -1) + 1 FROM script_turns WHERE script_id = ?1",
                        params![script_id_clone],
                        |r| r.get::<_, i64>(0),
                    )?;
                tx.execute(
                    "INSERT INTO script_turns (id, script_id, ord, role, content, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![turn_id_clone, script_id_clone, ord, role_clone, content_clone, now],
                )?;
                let mut block_rows = Vec::with_capacity(blocks_clone.len());
                for (i, (bid, text)) in block_ids_clone.iter().zip(blocks_clone.iter()).enumerate() {
                    tx.execute(
                        "INSERT INTO script_speech_blocks (id, turn_id, ord, text, selected_take)
                         VALUES (?1, ?2, ?3, ?4, NULL)",
                        params![bid, turn_id_clone, i as i64, text],
                    )?;
                    block_rows.push(ScriptBlockRow {
                        id: bid.clone(),
                        ord: i as i64,
                        text: text.clone(),
                        selected_take: None,
                        takes: Vec::new(),
                    });
                }
                tx.execute(
                    "UPDATE scripts SET updated_at = ?2 WHERE id = ?1",
                    params![script_id_clone, now],
                )?;
                tx.commit()?;
                Ok((ord, block_rows))
            },
        )
        .await
        .context("db task panicked")??;

        Ok(ScriptTurnRow {
            id: turn_id,
            ord,
            role,
            content,
            blocks: block_rows,
        })
    }

    /// Append a take (widget) to a speech block. `selected_take` is bumped to
    /// the new take's ord so the block auto-adopts the freshly rendered clip.
    /// Returns the take row.
    pub async fn append_take(
        &self,
        block_id: String,
        widget_id: String,
    ) -> Result<Option<ScriptTakeRow>> {
        let take_id = Uuid::new_v4().to_string();
        let now = unix_now();
        let db = self.db.clone();
        let take_id_clone = take_id.clone();
        let block_id_clone = block_id.clone();
        let widget_id_clone = widget_id.clone();
        let ord_opt = tokio::task::spawn_blocking(move || -> Result<Option<i64>> {
            let mut conn = db.lock().unwrap();
            let tx = conn.transaction()?;
            // Verify the block exists before writing so an orphan take id
            // isn't silently created against a missing block row.
            let exists: bool = tx
                .query_row(
                    "SELECT 1 FROM script_speech_blocks WHERE id = ?1",
                    params![block_id_clone],
                    |_| Ok(true),
                )
                .optional()?
                .unwrap_or(false);
            if !exists {
                return Ok(None);
            }
            let ord: i64 = tx
                .query_row(
                    "SELECT COALESCE(MAX(ord), -1) + 1 FROM script_speech_takes WHERE block_id = ?1",
                    params![block_id_clone],
                    |r| r.get::<_, i64>(0),
                )?;
            tx.execute(
                "INSERT INTO script_speech_takes (id, block_id, ord, widget_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![take_id_clone, block_id_clone, ord, widget_id_clone, now],
            )?;
            tx.execute(
                "UPDATE script_speech_blocks SET selected_take = ?2 WHERE id = ?1",
                params![block_id_clone, ord],
            )?;
            tx.commit()?;
            Ok(Some(ord))
        })
        .await
        .context("db task panicked")??;

        Ok(ord_opt.map(|ord| ScriptTakeRow {
            id: take_id,
            ord,
            widget_id,
        }))
    }

    /// Set the block's `selected_take`. `selected` is the take ordinal (as
    /// stored in `script_speech_takes.ord`). Rejects out-of-range values with
    /// UpdateResult::NotFound so the caller returns a clean 404.
    pub async fn set_selected_take(
        &self,
        block_id: String,
        selected: i64,
    ) -> Result<UpdateResult> {
        let db = self.db.clone();
        let block_id_clone = block_id.clone();
        let rows = tokio::task::spawn_blocking(move || -> Result<usize> {
            let conn = db.lock().unwrap();
            // Guard against selecting an ord that has no matching take.
            let exists: bool = conn
                .query_row(
                    "SELECT 1 FROM script_speech_takes
                       WHERE block_id = ?1 AND ord = ?2",
                    params![block_id_clone, selected],
                    |_| Ok(true),
                )
                .optional()?
                .unwrap_or(false);
            if !exists {
                return Ok(0);
            }
            let rows = conn.execute(
                "UPDATE script_speech_blocks SET selected_take = ?2 WHERE id = ?1",
                params![block_id_clone, selected],
            )?;
            Ok(rows)
        })
        .await
        .context("db task panicked")??;
        if rows == 0 {
            return Ok(UpdateResult::NotFound);
        }
        Ok(UpdateResult::Updated)
    }

    /// Delete a take and its widget. Returns the pair (block_id, widget_id)
    /// so the caller can cascade-delete the widget's WAV. If the take was
    /// the block's `selected_take`, we shift selection to the highest
    /// remaining ord (typically the previous take) or NULL if none remain.
    pub async fn delete_take(&self, take_id: String) -> Result<Option<DeletedTake>> {
        let db = self.db.clone();
        let take_id_clone = take_id.clone();
        let deleted = tokio::task::spawn_blocking(move || -> Result<Option<DeletedTake>> {
            let mut conn = db.lock().unwrap();
            let tx = conn.transaction()?;
            let row: Option<(String, i64, String)> = tx
                .query_row(
                    "SELECT block_id, ord, widget_id FROM script_speech_takes WHERE id = ?1",
                    params![take_id_clone],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)),
                )
                .optional()?;
            let Some((block_id, ord, widget_id)) = row else {
                return Ok(None);
            };
            tx.execute(
                "DELETE FROM script_speech_takes WHERE id = ?1",
                params![take_id_clone],
            )?;
            // Fix up selected_take if it pointed at the deleted ord.
            let cur_selected: Option<i64> = tx
                .query_row(
                    "SELECT selected_take FROM script_speech_blocks WHERE id = ?1",
                    params![block_id],
                    |r| r.get::<_, Option<i64>>(0),
                )
                .optional()?
                .flatten();
            if cur_selected == Some(ord) {
                let next: Option<i64> = tx
                    .query_row(
                        "SELECT MAX(ord) FROM script_speech_takes WHERE block_id = ?1",
                        params![block_id],
                        |r| r.get::<_, Option<i64>>(0),
                    )
                    .optional()?
                    .flatten();
                tx.execute(
                    "UPDATE script_speech_blocks SET selected_take = ?2 WHERE id = ?1",
                    params![block_id, next],
                )?;
            }
            tx.commit()?;
            Ok(Some(DeletedTake {
                block_id,
                widget_id,
            }))
        })
        .await
        .context("db task panicked")??;
        Ok(deleted)
    }

    /// Wipe every turn (and cascade-delete every block/take/widget) for a
    /// script. Returns the list of widget ids that were cascaded so the
    /// caller can drop their WAV files from disk. The script row itself
    /// stays — a fresh conversation starts empty in the same slot.
    pub async fn clear_script(&self, script_id: String) -> Result<Vec<String>> {
        let db = self.db.clone();
        let widget_ids = tokio::task::spawn_blocking(move || -> Result<Vec<String>> {
            let mut conn = db.lock().unwrap();
            let tx = conn.transaction()?;
            // Collect widget ids first so we can hand them back for WAV
            // cleanup — the DELETE below cascades the rows themselves.
            let mut stmt = tx.prepare(
                "SELECT t.widget_id
                   FROM script_speech_takes t
                   JOIN script_speech_blocks b ON b.id = t.block_id
                   JOIN script_turns tr        ON tr.id = b.turn_id
                  WHERE tr.script_id = ?1",
            )?;
            let ids: Vec<String> = stmt
                .query_map(params![script_id], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(stmt);
            tx.execute(
                "DELETE FROM script_turns WHERE script_id = ?1",
                params![script_id],
            )?;
            tx.commit()?;
            Ok(ids)
        })
        .await
        .context("db task panicked")??;
        Ok(widget_ids)
    }

    /// Look up the on-disk text of a speech block so a `POST .../takes` can
    /// re-render without the client having to echo the text back.
    pub async fn get_block_text(&self, block_id: String) -> Result<Option<String>> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<String>> {
            let conn = db.lock().unwrap();
            let text = conn
                .query_row(
                    "SELECT text FROM script_speech_blocks WHERE id = ?1",
                    params![block_id],
                    |r| r.get::<_, String>(0),
                )
                .optional()?;
            Ok(text)
        })
        .await
        .context("db task panicked")?
    }
}

pub enum UpdateResult {
    Updated,
    NotFound,
}

pub struct WidgetRow {
    pub sample_rate: u32,
    pub duration_ms: u64,
}

pub struct ImageRow {
    pub mime: String,
    pub byte_size: u64,
}

pub struct ScriptRow {
    pub id: String,
    pub turns: Vec<ScriptTurnRow>,
}

pub struct ScriptTurnRow {
    pub id: String,
    pub ord: i64,
    pub role: String,
    pub content: String,
    pub blocks: Vec<ScriptBlockRow>,
}

pub struct ScriptBlockRow {
    pub id: String,
    pub ord: i64,
    pub text: String,
    pub selected_take: Option<i64>,
    pub takes: Vec<ScriptTakeRow>,
}

pub struct ScriptTakeRow {
    pub id: String,
    pub ord: i64,
    pub widget_id: String,
}

pub struct DeletedTake {
    pub block_id: String,
    pub widget_id: String,
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
