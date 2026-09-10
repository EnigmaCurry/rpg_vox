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
    widget_id    TEXT REFERENCES widgets(id) ON DELETE SET NULL,
    created_at   INTEGER NOT NULL,
    UNIQUE(script_id, ord)
);
CREATE TABLE IF NOT EXISTS script_speech_blocks (
    id            TEXT PRIMARY KEY,
    turn_id       TEXT NOT NULL REFERENCES script_turns(id) ON DELETE CASCADE,
    ord           INTEGER NOT NULL,
    text          TEXT NOT NULL,
    role          TEXT NOT NULL DEFAULT 'character',
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
CREATE TABLE IF NOT EXISTS agents (
    id                TEXT PRIMARY KEY,
    name              TEXT NOT NULL,
    system_prompt     TEXT NOT NULL,
    project_id        TEXT,
    voice_user        TEXT,
    voice_narrator    TEXT,
    voice_character   TEXT,
    -- Wait-fill click preset id (see tts::clicks::ClickPreset). NULL =
    -- no click bed while the LLM is deliberating; a non-NULL string
    -- like "vintage" selects a preset. Nullable so existing agents
    -- default off — quiet-by-default is the least-intrusive migration.
    interstitial      TEXT,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL
);
-- Compact voice-prompt files (the output of Qwen3 /save_prompt). Bytes on
-- disk under `data/voices/{id}.bin`; the row is the manifest. `filename`
-- preserves the extension the Gradio server handed us so we can round-
-- trip it on future uploads without guessing.
CREATE TABLE IF NOT EXISTS voices (
    id           TEXT PRIMARY KEY,
    filename     TEXT NOT NULL,
    byte_size    INTEGER NOT NULL,
    created_at   INTEGER NOT NULL
);
"#;

/// Single hardcoded script id — MVP has one shared conversation, matching
/// the legacy /chat semantics. Multi-script (per-project) comes later.
pub const DEFAULT_SCRIPT_ID: &str = "default";

/// Well-known id for the built-in "Default" agent seeded from
/// prompts/script.txt on first boot. Frontend hard-codes this in the
/// dropdown so it's always selectable even after user creates others.
pub const DEFAULT_AGENT_ID: &str = "default";

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
    voices_dir: PathBuf,
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
        let voices_dir = data_dir.join("voices");
        std::fs::create_dir_all(&voices_dir)
            .with_context(|| format!("creating voices dir {}", voices_dir.display()))?;

        let db_path = data_dir.join("rpg_vox.sqlite");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("opening sqlite at {}", db_path.display()))?;
        // ON DELETE CASCADE only fires when foreign_keys pragma is on; sqlite
        // defaults it off per-connection. Set before the schema applies so any
        // downstream migration can rely on FK checks too.
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .context("enabling foreign_keys pragma")?;
        conn.execute_batch(SCHEMA).context("applying schema")?;

        // Idempotent per-column migrations. `CREATE TABLE IF NOT EXISTS`
        // above is a no-op when the table already exists at an older
        // schema — so any column added after the initial cut needs an
        // `ALTER TABLE ADD COLUMN` guarded by a `pragma_table_info` probe.
        // Kept inline (instead of a migrations dir) because the count is
        // tiny and each one is one line.
        add_column_if_missing(
            &conn,
            "script_speech_blocks",
            "role",
            "TEXT NOT NULL DEFAULT 'character'",
        )
        .context("migrating role column on script_speech_blocks")?;
        // User turns can carry an attached widget (a recorded voice memo
        // captured from the mic before send). Nullable — plain text turns
        // stay unchanged, and assistant turns never use this column.
        add_column_if_missing(
            &conn,
            "script_turns",
            "widget_id",
            "TEXT REFERENCES widgets(id) ON DELETE SET NULL",
        )
        .context("migrating widget_id column on script_turns")?;
        // Agents gained a project scope + per-role voice slots after the
        // initial cut. All four are nullable — the built-in Default keeps
        // NULL project_id (global fallback), and unset voice slots fall
        // back to the hardcoded DSP defaults so pre-migration agents
        // sound unchanged.
        for (col, decl) in [
            ("project_id", "TEXT"),
            ("voice_user", "TEXT"),
            ("voice_narrator", "TEXT"),
            ("voice_character", "TEXT"),
            // Per-agent "computer is thinking" click preset. See the
            // agents table body above for the semantics.
            ("interstitial", "TEXT"),
        ] {
            add_column_if_missing(&conn, "agents", col, decl)
                .with_context(|| format!("migrating {col} column on agents"))?;
        }

        Ok(Self {
            db: Arc::new(Mutex::new(conn)),
            clips_dir,
            images_dir,
            voices_dir,
        })
    }

    pub fn clip_path(&self, id: &str) -> PathBuf {
        self.clips_dir.join(format!("{id}.wav"))
    }

    pub fn image_path(&self, id: &str) -> PathBuf {
        self.images_dir.join(id)
    }

    pub fn voice_path(&self, id: &str) -> PathBuf {
        self.voices_dir.join(id)
    }

    /// Path to the original reference wav that produced this voice-prompt
    /// file. Kept for user review (a play button in the Characters UI so
    /// people can hear what they uploaded); NEVER used by the synth path.
    pub fn voice_reference_path(&self, id: &str) -> PathBuf {
        self.voices_dir.join(format!("{id}.wav"))
    }

    /// Persist a compact voice-prompt file (bytes returned from Qwen3
    /// `/save_prompt`) alongside the original reference wav. `filename`
    /// is preserved verbatim so we can round-trip it on future Gradio
    /// uploads (the extension matters — .bin vs .pt). `reference_wav`
    /// is optional: when None we skip writing the review-playback file
    /// (e.g. an admin-uploaded voice-prompt without an original recording).
    pub async fn create_voice(
        &self,
        bytes: Vec<u8>,
        filename: String,
        reference_wav: Option<Vec<u8>>,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = unix_now();
        let byte_size = bytes.len() as i64;
        let db = self.db.clone();
        let id_clone = id.clone();
        let filename_clone = filename.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO voices (id, filename, byte_size, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![id_clone, filename_clone, byte_size, now],
            )?;
            Ok(())
        })
        .await
        .context("db task panicked")??;

        tokio::fs::write(self.voice_path(&id), bytes)
            .await
            .with_context(|| format!("writing voice bytes for {id}"))?;
        if let Some(wav) = reference_wav {
            tokio::fs::write(self.voice_reference_path(&id), wav)
                .await
                .with_context(|| format!("writing reference wav for {id}"))?;
        }

        Ok(id)
    }

    /// Read a voice-prompt file back off disk alongside its stored filename.
    /// Returns `Ok(None)` when the row doesn't exist so callers can
    /// distinguish "unknown id" from a real I/O error.
    pub async fn get_voice_bytes(&self, id: String) -> Result<Option<(Vec<u8>, String)>> {
        let db = self.db.clone();
        let id_clone = id.clone();
        let row: Option<String> = tokio::task::spawn_blocking(move || -> Result<Option<String>> {
            let conn = db.lock().unwrap();
            let row = conn
                .query_row(
                    "SELECT filename FROM voices WHERE id = ?1",
                    params![id_clone],
                    |r| r.get::<_, String>(0),
                )
                .optional()?;
            Ok(row)
        })
        .await
        .context("db task panicked")??;
        let Some(filename) = row else {
            return Ok(None);
        };
        let bytes = tokio::fs::read(self.voice_path(&id))
            .await
            .with_context(|| format!("reading voice bytes for {id}"))?;
        Ok(Some((bytes, filename)))
    }

    /// Drop a voice by id: DB row first, then the on-disk file. Missing
    /// file is not an error (partial state from a crash mid-write); a
    /// missing row is reported via `UpdateResult::NotFound`.
    pub async fn delete_voice(&self, id: String) -> Result<UpdateResult> {
        let db = self.db.clone();
        let id_clone = id.clone();
        let rows = tokio::task::spawn_blocking(move || -> Result<usize> {
            let conn = db.lock().unwrap();
            let rows = conn.execute("DELETE FROM voices WHERE id = ?1", params![id_clone])?;
            Ok(rows)
        })
        .await
        .context("db task panicked")??;
        let _ = tokio::fs::remove_file(self.voice_path(&id)).await;
        let _ = tokio::fs::remove_file(self.voice_reference_path(&id)).await;
        Ok(if rows == 0 {
            UpdateResult::NotFound
        } else {
            UpdateResult::Updated
        })
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

    /// Insert the default script row if the scripts table is entirely
    /// empty. Called at boot so a fresh install lands with one starter
    /// script the user can click into. Existing installs already have at
    /// least one script and this is a no-op.
    pub async fn ensure_default_script(&self) -> Result<()> {
        let now = unix_now();
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = db.lock().unwrap();
            let count: i64 =
                conn.query_row("SELECT COUNT(*) FROM scripts", [], |r| r.get(0))?;
            if count == 0 {
                conn.execute(
                    "INSERT INTO scripts (id, name, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![DEFAULT_SCRIPT_ID, "New script", now, now],
                )?;
            }
            Ok(())
        })
        .await
        .context("db task panicked")??;
        Ok(())
    }

    /// List all scripts ordered by most-recently updated first. Small
    /// projection (id + name + updated_at) — the full turn/block/take
    /// hydration only happens on demand via `get_script`.
    pub async fn list_scripts(&self) -> Result<Vec<ScriptSummaryRow>> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<ScriptSummaryRow>> {
            let conn = db.lock().unwrap();
            let mut stmt = conn.prepare(
                "SELECT id, name, created_at, updated_at
                   FROM scripts
                  ORDER BY updated_at DESC",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(ScriptSummaryRow {
                        id: r.get::<_, String>(0)?,
                        name: r.get::<_, String>(1)?,
                        created_at: r.get::<_, i64>(2)?,
                        updated_at: r.get::<_, i64>(3)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
        .context("db task panicked")?
    }

    /// Create a new empty script with the given name.
    pub async fn create_script(&self, name: String) -> Result<ScriptSummaryRow> {
        let id = Uuid::new_v4().to_string();
        let now = unix_now();
        let db = self.db.clone();
        let id_clone = id.clone();
        let name_clone = name.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO scripts (id, name, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![id_clone, name_clone, now, now],
            )?;
            Ok(())
        })
        .await
        .context("db task panicked")??;
        Ok(ScriptSummaryRow {
            id,
            name,
            created_at: now,
            updated_at: now,
        })
    }

    /// Rename an existing script. `NotFound` when the id doesn't exist.
    pub async fn rename_script(&self, id: String, name: String) -> Result<UpdateResult> {
        let now = unix_now();
        let db = self.db.clone();
        let rows = tokio::task::spawn_blocking(move || -> Result<usize> {
            let conn = db.lock().unwrap();
            let rows = conn.execute(
                "UPDATE scripts SET name = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, name, now],
            )?;
            Ok(rows)
        })
        .await
        .context("db task panicked")??;
        Ok(if rows == 0 {
            UpdateResult::NotFound
        } else {
            UpdateResult::Updated
        })
    }

    /// Delete a script and cascade its turns/blocks/takes. Returns the
    /// widget ids that were attached (user-turn recordings + GM proxies +
    /// take widgets) so the caller can drop their WAV files. The script
    /// row itself goes with it (unlike `clear_script`, which keeps the
    /// row and just wipes contents).
    pub async fn delete_script(&self, script_id: String) -> Result<Vec<String>> {
        let db = self.db.clone();
        let widget_ids = tokio::task::spawn_blocking(move || -> Result<Vec<String>> {
            let mut conn = db.lock().unwrap();
            let tx = conn.transaction()?;
            let mut ids: Vec<String> = Vec::new();
            {
                let mut stmt = tx.prepare(
                    "SELECT t.widget_id
                       FROM script_speech_takes t
                       JOIN script_speech_blocks b ON b.id = t.block_id
                       JOIN script_turns tr        ON tr.id = b.turn_id
                      WHERE tr.script_id = ?1",
                )?;
                let rows = stmt
                    .query_map(params![script_id], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                ids.extend(rows);
            }
            {
                let mut stmt = tx.prepare(
                    "SELECT widget_id FROM script_turns
                      WHERE script_id = ?1 AND widget_id IS NOT NULL",
                )?;
                let rows = stmt
                    .query_map(params![script_id], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                ids.extend(rows);
            }
            // FK cascade on script_turns.script_id drops turns → blocks →
            // takes when we drop the script row.
            tx.execute("DELETE FROM scripts WHERE id = ?1", params![script_id])?;
            tx.commit()?;
            Ok(ids)
        })
        .await
        .context("db task panicked")??;
        Ok(widget_ids)
    }

    /// Look up just a script's display name. `Ok(None)` when the id doesn't
    /// exist. Used by callers that need the name for a filename or heading
    /// without the cost of hydrating every turn + block + take.
    pub async fn get_script_name(&self, script_id: String) -> Result<Option<String>> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<String>> {
            let conn = db.lock().unwrap();
            let row = conn
                .query_row(
                    "SELECT name FROM scripts WHERE id = ?1",
                    params![script_id],
                    |r| r.get::<_, String>(0),
                )
                .optional()?;
            Ok(row)
        })
        .await
        .context("db task panicked")?
    }

    /// Read the full script (turns + blocks + takes) in ordering-stable form.
    /// Empty script returns Ok with an empty turns list.
    pub async fn get_script(&self, script_id: String) -> Result<ScriptRow> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || -> Result<ScriptRow> {
            let conn = db.lock().unwrap();
            let mut turns_stmt = conn.prepare(
                "SELECT id, ord, role, content, widget_id FROM script_turns
                 WHERE script_id = ?1 ORDER BY ord ASC",
            )?;
            let turns_iter = turns_stmt.query_map(params![script_id], |r| {
                Ok(ScriptTurnRow {
                    id: r.get::<_, String>(0)?,
                    ord: r.get::<_, i64>(1)?,
                    role: r.get::<_, String>(2)?,
                    content: r.get::<_, String>(3)?,
                    widget_id: r.get::<_, Option<String>>(4)?,
                    blocks: Vec::new(),
                })
            })?;
            let mut turns: Vec<ScriptTurnRow> =
                turns_iter.collect::<rusqlite::Result<Vec<_>>>()?;

            for turn in turns.iter_mut() {
                let mut block_stmt = conn.prepare(
                    "SELECT id, ord, text, role, selected_take
                     FROM script_speech_blocks
                     WHERE turn_id = ?1 ORDER BY ord ASC",
                )?;
                let blocks_iter = block_stmt.query_map(params![turn.id], |r| {
                    Ok(ScriptBlockRow {
                        id: r.get::<_, String>(0)?,
                        ord: r.get::<_, i64>(1)?,
                        text: r.get::<_, String>(2)?,
                        role: r.get::<_, String>(3)?,
                        selected_take: r.get::<_, Option<i64>>(4)?,
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
    ///
    /// Each entry in `blocks` is `(role, text)` where role is "narrator"
    /// or "character" — the parser tags prose outside `<speak>` as narrator
    /// and `<speak>` bodies as character so downstream take rendering can
    /// pick the right voice profile.
    pub async fn create_turn(
        &self,
        script_id: String,
        role: String,
        content: String,
        widget_id: Option<String>,
        blocks: Vec<(String, String)>,
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
        let widget_id_clone = widget_id.clone();
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
                    "INSERT INTO script_turns
                       (id, script_id, ord, role, content, widget_id, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![turn_id_clone, script_id_clone, ord, role_clone, content_clone, widget_id_clone, now],
                )?;
                let mut block_rows = Vec::with_capacity(blocks_clone.len());
                for (i, (bid, (block_role, text))) in
                    block_ids_clone.iter().zip(blocks_clone.iter()).enumerate()
                {
                    tx.execute(
                        "INSERT INTO script_speech_blocks
                           (id, turn_id, ord, text, role, selected_take)
                         VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
                        params![bid, turn_id_clone, i as i64, text, block_role],
                    )?;
                    block_rows.push(ScriptBlockRow {
                        id: bid.clone(),
                        ord: i as i64,
                        text: text.clone(),
                        role: block_role.clone(),
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
            widget_id,
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

    /// Keep only the most recent `keep` takes for a block, deleting the
    /// older ones. Returns the widget ids of the deleted takes so the
    /// caller can drop their WAV files + widget rows. `selected_take`
    /// stays valid because `append_take` always writes it to the newest
    /// ord — which is by definition never in the pruning window.
    pub async fn prune_block_takes(
        &self,
        block_id: String,
        keep: usize,
    ) -> Result<Vec<String>> {
        let db = self.db.clone();
        let widget_ids = tokio::task::spawn_blocking(move || -> Result<Vec<String>> {
            let mut conn = db.lock().unwrap();
            let tx = conn.transaction()?;
            // Anything past the newest `keep` (sorted by ord DESC) goes.
            let mut stmt = tx.prepare(
                "SELECT id, widget_id
                   FROM script_speech_takes
                  WHERE block_id = ?1
                  ORDER BY ord DESC
                  LIMIT -1 OFFSET ?2",
            )?;
            let victims: Vec<(String, String)> = stmt
                .query_map(params![block_id, keep as i64], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(stmt);
            let widget_ids: Vec<String> =
                victims.iter().map(|(_, w)| w.clone()).collect();
            for (take_id, _) in &victims {
                tx.execute(
                    "DELETE FROM script_speech_takes WHERE id = ?1",
                    params![take_id],
                )?;
            }
            tx.commit()?;
            Ok(widget_ids)
        })
        .await
        .context("db task panicked")??;
        Ok(widget_ids)
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
    ///
    /// Collects widget ids from TWO paths so nothing leaks:
    ///   1. Assistant-turn speech takes (character/narrator TTS clips).
    ///   2. User-turn `widget_id` (recorded voice memos + GM proxy synths).
    /// The FK cascade drops the rows, but WAV files live on disk under
    /// `data/clips/` and the caller has to unlink them explicitly.
    pub async fn clear_script(&self, script_id: String) -> Result<Vec<String>> {
        let db = self.db.clone();
        let widget_ids = tokio::task::spawn_blocking(move || -> Result<Vec<String>> {
            let mut conn = db.lock().unwrap();
            let tx = conn.transaction()?;
            let mut ids: Vec<String> = Vec::new();

            // (1) Widgets referenced by assistant-turn speech takes.
            {
                let mut stmt = tx.prepare(
                    "SELECT t.widget_id
                       FROM script_speech_takes t
                       JOIN script_speech_blocks b ON b.id = t.block_id
                       JOIN script_turns tr        ON tr.id = b.turn_id
                      WHERE tr.script_id = ?1",
                )?;
                let rows = stmt
                    .query_map(params![script_id], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                ids.extend(rows);
            }

            // (2) Widgets attached directly to user turns (recorded voice
            //     memos + GM proxy synths). `ON DELETE SET NULL` on the FK
            //     means these DON'T auto-cascade with the turn delete, so
            //     we hand them back to the caller for widget + WAV cleanup
            //     via `store.delete_widget` (which drops the row + file).
            {
                let mut stmt = tx.prepare(
                    "SELECT widget_id FROM script_turns
                      WHERE script_id = ?1 AND widget_id IS NOT NULL",
                )?;
                let rows = stmt
                    .query_map(params![script_id], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                ids.extend(rows);
            }

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

    /// Truncate a script at `turn_id`: delete that turn AND every turn with
    /// a greater `ord` within the same script. Returns the widget ids of
    /// everything that was cascaded so the caller can drop the WAV files
    /// (same contract as [`Self::clear_script`]). Ok(None) means the turn
    /// wasn't found (already deleted, or belongs to a different script) —
    /// the caller treats that as a NOT_FOUND without touching the store
    /// further.
    ///
    /// Used by the /scripts/:id/turns/:turn_id/edit-user flow so editing an
    /// earlier user turn rewinds the transcript to that point before the
    /// re-synth + LLM reply. The turn_id itself is dropped along with its
    /// followers so the caller can immediately insert a REPLACEMENT user
    /// turn at the freed ord.
    pub async fn truncate_script_from_turn(
        &self,
        script_id: String,
        turn_id: String,
    ) -> Result<Option<Vec<String>>> {
        let db = self.db.clone();
        let script_id_clone = script_id.clone();
        let turn_id_clone = turn_id.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<Vec<String>>> {
            let mut conn = db.lock().unwrap();
            let tx = conn.transaction()?;

            // Resolve the turn's ord. Missing → caller reports 404 and
            // leaves the script untouched.
            let ord: Option<i64> = tx
                .query_row(
                    "SELECT ord FROM script_turns
                      WHERE id = ?1 AND script_id = ?2",
                    params![turn_id_clone, script_id_clone],
                    |r| r.get::<_, i64>(0),
                )
                .optional()?;
            let Some(cutoff_ord) = ord else {
                return Ok(None);
            };

            let mut ids: Vec<String> = Vec::new();
            // Speech-take widgets for every turn at/after the cutoff. Same
            // shape as clear_script — narrator/character TTS clips are
            // linked through script_speech_blocks + script_speech_takes and
            // have an ON DELETE CASCADE up through the block, but the
            // widget rows live outside the cascade path.
            {
                let mut stmt = tx.prepare(
                    "SELECT t.widget_id
                       FROM script_speech_takes t
                       JOIN script_speech_blocks b ON b.id = t.block_id
                       JOIN script_turns tr        ON tr.id = b.turn_id
                      WHERE tr.script_id = ?1 AND tr.ord >= ?2",
                )?;
                let rows = stmt
                    .query_map(params![script_id_clone, cutoff_ord], |r| {
                        r.get::<_, String>(0)
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                ids.extend(rows);
            }
            // User-turn attached widgets (recording + GM proxy synth) —
            // ON DELETE SET NULL on the FK, so the caller has to unlink.
            {
                let mut stmt = tx.prepare(
                    "SELECT widget_id FROM script_turns
                      WHERE script_id = ?1 AND ord >= ?2
                        AND widget_id IS NOT NULL",
                )?;
                let rows = stmt
                    .query_map(params![script_id_clone, cutoff_ord], |r| {
                        r.get::<_, String>(0)
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                ids.extend(rows);
            }

            tx.execute(
                "DELETE FROM script_turns
                  WHERE script_id = ?1 AND ord >= ?2",
                params![script_id_clone, cutoff_ord],
            )?;
            tx.execute(
                "UPDATE scripts SET updated_at = ?2 WHERE id = ?1",
                params![script_id_clone, unix_now()],
            )?;
            tx.commit()?;
            Ok(Some(ids))
        })
        .await
        .context("db task panicked")?
    }

    /// Look up a block's persisted text + role so `POST .../takes` can
    /// re-render without the client echoing the text back, and can pick a
    /// role-appropriate voice profile automatically.
    pub async fn get_block_info(&self, block_id: String) -> Result<Option<BlockInfo>> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<BlockInfo>> {
            let conn = db.lock().unwrap();
            let row = conn
                .query_row(
                    "SELECT text, role FROM script_speech_blocks WHERE id = ?1",
                    params![block_id],
                    |r| {
                        Ok(BlockInfo {
                            text: r.get::<_, String>(0)?,
                            role: r.get::<_, String>(1)?,
                        })
                    },
                )
                .optional()?;
            Ok(row)
        })
        .await
        .context("db task panicked")?
    }
}

pub struct BlockInfo {
    pub text: String,
    pub role: String,
}

pub struct AgentRow {
    pub id: String,
    pub name: String,
    pub system_prompt: String,
    /// NULL for the built-in Default agent (shown in every project's list).
    /// Set for user-created agents so the picker can filter by project.
    pub project_id: Option<String>,
    /// Character ids that voice each role's synth. NULL falls back to the
    /// hardcoded DSP defaults (gm_proxy_configs / default_configs_for_role).
    pub voice_user: Option<String>,
    pub voice_narrator: Option<String>,
    pub voice_character: Option<String>,
    /// Wait-fill "computer is thinking" click preset id. NULL = no click
    /// bed. See [`crate::tts::clicks::ClickPreset::from_str_id`] for the
    /// current whitelist of strings the runner accepts.
    pub interstitial: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Which patchable field of an agent row is being set. Voice slots use
/// `Option<Option<String>>` at the API layer (see `update_agent`) so callers
/// can distinguish "leave alone" from "clear to NULL"; this enum represents
/// only the fields the caller wants to change.
pub enum AgentField {
    Name(String),
    SystemPrompt(String),
    VoiceUser(Option<String>),
    VoiceNarrator(Option<String>),
    VoiceCharacter(Option<String>),
    /// Preset id (or None to clear back to "no click bed"). Validated at
    /// the HTTP layer against [`crate::tts::clicks::ClickPreset::from_str_id`]
    /// before it reaches the store, so any value here is trusted.
    Interstitial(Option<String>),
}

fn row_to_agent(r: &rusqlite::Row<'_>) -> rusqlite::Result<AgentRow> {
    Ok(AgentRow {
        id: r.get::<_, String>(0)?,
        name: r.get::<_, String>(1)?,
        system_prompt: r.get::<_, String>(2)?,
        project_id: r.get::<_, Option<String>>(3)?,
        voice_user: r.get::<_, Option<String>>(4)?,
        voice_narrator: r.get::<_, Option<String>>(5)?,
        voice_character: r.get::<_, Option<String>>(6)?,
        interstitial: r.get::<_, Option<String>>(7)?,
        created_at: r.get::<_, i64>(8)?,
        updated_at: r.get::<_, i64>(9)?,
    })
}

impl Store {
    /// Sync the built-in "Default" agent from `prompts/script.txt` on
    /// every boot. Default is read-only in the API — users create their
    /// own agents to customize — so keeping it locked to the on-disk
    /// prompt file means file edits (git pulls, local tweaks) actually
    /// take effect rather than being frozen at first-boot values.
    /// User-created agents are completely untouched by this.
    pub async fn ensure_default_agent(&self, seed_prompt: String) -> Result<()> {
        let now = unix_now();
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = db.lock().unwrap();
            // Default agent stays project-less (NULL) and voice-less so it
            // shows up in every project's picker and falls back to the
            // hardcoded DSP presets. Only refresh name + prompt on boot.
            conn.execute(
                "INSERT INTO agents
                   (id, name, system_prompt, project_id,
                    voice_user, voice_narrator, voice_character,
                    interstitial,
                    created_at, updated_at)
                 VALUES (?1, ?2, ?3, NULL, NULL, NULL, NULL, NULL, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET
                     name = excluded.name,
                     system_prompt = excluded.system_prompt,
                     updated_at = excluded.updated_at",
                params![DEFAULT_AGENT_ID, "Default", seed_prompt, now, now],
            )?;
            Ok(())
        })
        .await
        .context("db task panicked")??;
        Ok(())
    }

    /// List agents visible to `project_id`: rows with a matching project plus
    /// the built-in Default (which has NULL project_id and is always visible).
    /// Passing `None` returns every agent — useful for admin/debug callers.
    pub async fn list_agents(&self, project_id: Option<String>) -> Result<Vec<AgentRow>> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<AgentRow>> {
            let conn = db.lock().unwrap();
            let rows: Vec<AgentRow> = if let Some(pid) = project_id {
                let mut stmt = conn.prepare(
                    "SELECT id, name, system_prompt, project_id,
                            voice_user, voice_narrator, voice_character,
                            interstitial,
                            created_at, updated_at
                       FROM agents
                      WHERE project_id IS NULL OR project_id = ?2
                      ORDER BY (id != ?1) ASC, name COLLATE NOCASE ASC",
                )?;
                let rows = stmt
                    .query_map(params![DEFAULT_AGENT_ID, pid], row_to_agent)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            } else {
                let mut stmt = conn.prepare(
                    "SELECT id, name, system_prompt, project_id,
                            voice_user, voice_narrator, voice_character,
                            interstitial,
                            created_at, updated_at
                       FROM agents
                      ORDER BY (id != ?1) ASC, name COLLATE NOCASE ASC",
                )?;
                let rows = stmt
                    .query_map(params![DEFAULT_AGENT_ID], row_to_agent)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            Ok(rows)
        })
        .await
        .context("db task panicked")?
    }

    pub async fn get_agent(&self, id: String) -> Result<Option<AgentRow>> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<AgentRow>> {
            let conn = db.lock().unwrap();
            let row = conn
                .query_row(
                    "SELECT id, name, system_prompt, project_id,
                            voice_user, voice_narrator, voice_character,
                            interstitial,
                            created_at, updated_at
                       FROM agents WHERE id = ?1",
                    params![id],
                    row_to_agent,
                )
                .optional()?;
            Ok(row)
        })
        .await
        .context("db task panicked")?
    }

    /// Create a new agent with the given name and project. Seeded with the
    /// Default agent's current system prompt so users don't start from scratch
    /// — they can then edit anything, including reverting to blank. Voice
    /// slots start NULL (falling back to the hardcoded DSP presets) until
    /// the user picks characters in the agent editor.
    pub async fn create_agent(
        &self,
        name: String,
        project_id: Option<String>,
    ) -> Result<AgentRow> {
        let id = Uuid::new_v4().to_string();
        let now = unix_now();
        let db = self.db.clone();
        let id_clone = id.clone();
        let name_clone = name.clone();
        let project_clone = project_id.clone();
        let seed = tokio::task::spawn_blocking(move || -> Result<String> {
            let conn = db.lock().unwrap();
            let seed: String = conn
                .query_row(
                    "SELECT system_prompt FROM agents WHERE id = ?1",
                    params![DEFAULT_AGENT_ID],
                    |r| r.get::<_, String>(0),
                )
                .optional()?
                .unwrap_or_default();
            conn.execute(
                "INSERT INTO agents
                   (id, name, system_prompt, project_id,
                    voice_user, voice_narrator, voice_character,
                    interstitial,
                    created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, NULL, NULL, NULL, NULL, ?5, ?6)",
                params![id_clone, name_clone, seed, project_clone, now, now],
            )?;
            Ok(seed)
        })
        .await
        .context("db task panicked")??;
        Ok(AgentRow {
            id,
            name,
            system_prompt: seed,
            project_id,
            voice_user: None,
            voice_narrator: None,
            voice_character: None,
            interstitial: None,
            created_at: now,
            updated_at: now,
        })
    }

    /// Patch any subset of an agent's fields. Only fields present in
    /// `changes` are written; voice slots use `Option<Option<String>>` at
    /// the API layer to distinguish "leave alone" (outer None) from "clear
    /// to NULL" (outer Some(None)). Returns UpdateResult::NotFound if the
    /// row doesn't exist.
    pub async fn update_agent(
        &self,
        id: String,
        changes: Vec<AgentField>,
    ) -> Result<UpdateResult> {
        if changes.is_empty() {
            return Ok(UpdateResult::Updated);
        }
        let now = unix_now();
        let db = self.db.clone();
        let rows = tokio::task::spawn_blocking(move || -> Result<usize> {
            let conn = db.lock().unwrap();
            let mut sets: Vec<&str> = Vec::new();
            let mut vals: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
            for change in changes {
                match change {
                    AgentField::Name(n) => { sets.push("name = ?"); vals.push(Box::new(n)); }
                    AgentField::SystemPrompt(p) => { sets.push("system_prompt = ?"); vals.push(Box::new(p)); }
                    AgentField::VoiceUser(v) => { sets.push("voice_user = ?"); vals.push(Box::new(v)); }
                    AgentField::VoiceNarrator(v) => { sets.push("voice_narrator = ?"); vals.push(Box::new(v)); }
                    AgentField::VoiceCharacter(v) => { sets.push("voice_character = ?"); vals.push(Box::new(v)); }
                    AgentField::Interstitial(v) => { sets.push("interstitial = ?"); vals.push(Box::new(v)); }
                }
            }
            sets.push("updated_at = ?");
            vals.push(Box::new(now));
            vals.push(Box::new(id));
            let sql = format!(
                "UPDATE agents SET {} WHERE id = ?",
                sets.join(", "),
            );
            let params_refs: Vec<&dyn rusqlite::ToSql> =
                vals.iter().map(|b| b.as_ref()).collect();
            let rows = conn.execute(&sql, params_refs.as_slice())?;
            Ok(rows)
        })
        .await
        .context("db task panicked")??;
        Ok(if rows == 0 {
            UpdateResult::NotFound
        } else {
            UpdateResult::Updated
        })
    }

    pub async fn delete_agent(&self, id: String) -> Result<UpdateResult> {
        let db = self.db.clone();
        let rows = tokio::task::spawn_blocking(move || -> Result<usize> {
            let conn = db.lock().unwrap();
            let rows = conn.execute("DELETE FROM agents WHERE id = ?1", params![id])?;
            Ok(rows)
        })
        .await
        .context("db task panicked")??;
        Ok(if rows == 0 {
            UpdateResult::NotFound
        } else {
            UpdateResult::Updated
        })
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
    /// Attached widget (recorded voice memo) for user turns entered via the
    /// microphone. Assistant turns leave this null; a user turn typed at the
    /// keyboard also has null here.
    pub widget_id: Option<String>,
    pub blocks: Vec<ScriptBlockRow>,
}

pub struct ScriptBlockRow {
    pub id: String,
    pub ord: i64,
    pub text: String,
    /// "narrator" (prose outside `<speak>` — synthesized in the storyteller
    /// voice) or "character" (`<speak>` body — the PC's in-character line).
    /// Kept as `String` at this layer so the store doesn't force an enum on
    /// callers; http.rs and script.rs use a typed `Role` at their edges.
    pub role: String,
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

pub struct ScriptSummaryRow {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub updated_at: i64,
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Add `column` to `table` when it isn't already there. `col_def` is the SQL
/// fragment after the column name (type + constraints + default). Fails on
/// any error other than "the column exists" — safe to call on every startup.
fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    col_def: &str,
) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let existing: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if existing.iter().any(|c| c == column) {
        return Ok(());
    }
    conn.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} {col_def}"),
        [],
    )?;
    Ok(())
}
