//! Persistent store for designed speech clips.
//!
//! Two backing pieces, both under a single `--data-dir` (default `./data`):
//!
//! * `data/rpg_vox.sqlite` — one row per widget (id, text, instruct, clip
//!   metadata). Accessed through rusqlite behind a Mutex; every call is
//!   wrapped in `spawn_blocking` so the tokio runtime doesn't stall on
//!   sqlite I/O.
//! * `data/clips/{id}.wav` — the rendered PCM as WAV, one file per widget.
//!   Streamed to the browser on render and deleted alongside the DB row
//!   on delete.
//!
//! No caching layer over sqlite — the widget count in this app is small and
//! every access is user-triggered, so the extra complexity isn't worth it.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

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
"#;

#[derive(Clone)]
pub struct Store {
    db: Arc<Mutex<Connection>>,
    clips_dir: PathBuf,
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

        let db_path = data_dir.join("rpg_vox.sqlite");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("opening sqlite at {}", db_path.display()))?;
        conn.execute_batch(SCHEMA).context("applying schema")?;

        Ok(Self {
            db: Arc::new(Mutex::new(conn)),
            clips_dir,
        })
    }

    pub fn clip_path(&self, id: &str) -> PathBuf {
        self.clips_dir.join(format!("{id}.wav"))
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
}

pub enum UpdateResult {
    Updated,
    NotFound,
}

pub struct WidgetRow {
    pub sample_rate: u32,
    pub duration_ms: u64,
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
