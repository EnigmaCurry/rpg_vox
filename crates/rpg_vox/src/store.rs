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
"#;

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

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
