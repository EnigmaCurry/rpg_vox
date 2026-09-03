//! Runtime-mutable settings shared between HTTP handlers and worker tasks.
//!
//! Seeded from CLI args at startup; edited live via `POST /settings`. Not
//! persisted — the CLI is the source of truth on restart.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone, Debug, Serialize)]
pub struct Settings {
    pub comfyui_base: String,
}

#[derive(Debug, Deserialize)]
pub struct SettingsUpdate {
    pub comfyui_base: Option<String>,
}

impl Settings {
    /// Applies a partial update in place. Returns whichever fields ended up
    /// changed (unused for now; useful when we add more settings).
    pub fn apply(&mut self, update: SettingsUpdate) {
        if let Some(v) = update.comfyui_base {
            let v = v.trim().trim_end_matches('/').to_string();
            if !v.is_empty() {
                self.comfyui_base = v;
            }
        }
    }
}

pub type Shared = Arc<RwLock<Settings>>;

pub fn new(settings: Settings) -> Shared {
    Arc::new(RwLock::new(settings))
}
