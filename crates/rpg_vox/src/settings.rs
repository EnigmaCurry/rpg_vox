//! Runtime-mutable settings shared between HTTP handlers and worker tasks.
//!
//! Seeded from CLI args / env vars at startup. `POST /settings` can edit the
//! workflow selection; the ComfyUI endpoint is fixed at startup and is not
//! exposed as mutable state.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::workflow::{self, Registry, WorkflowSummary};

#[derive(Clone, Debug)]
pub struct Settings {
    pub comfyui_base: String,
    /// Selected workflow name (registry key), or None for the built-in placeholder.
    pub workflow_name: Option<String>,
    /// Parsed workflow JSON; not exposed over the wire.
    pub workflow_json: Value,
    pub workflow_summary: WorkflowSummary,
}

/// Wire type exposed by `GET /settings`.
#[derive(Clone, Debug, Serialize)]
pub struct SettingsPublic {
    pub workflow_name: Option<String>,
    pub workflow_summary: WorkflowSummary,
}

impl Settings {
    pub fn public(&self) -> SettingsPublic {
        SettingsPublic {
            workflow_name: self.workflow_name.clone(),
            workflow_summary: self.workflow_summary.clone(),
        }
    }

    /// Apply a partial update. `registry` is consulted when switching
    /// workflows — an unknown name is an error.
    pub fn apply(&mut self, update: SettingsUpdate, registry: &Registry) -> Result<(), String> {
        if let Some(name) = update.workflow_name {
            let trimmed = name.trim().to_string();
            if trimmed.is_empty() {
                let (json, summary) = workflow::builtin_placeholder();
                self.workflow_json = json;
                self.workflow_summary = summary;
                self.workflow_name = None;
            } else {
                let entry = registry
                    .get(&trimmed)
                    .ok_or_else(|| format!("no workflow named '{trimmed}' in registry"))?;
                self.workflow_json = entry.json.clone();
                self.workflow_summary = entry.summary.clone();
                self.workflow_name = Some(entry.name.clone());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
pub struct SettingsUpdate {
    /// Registry key (file stem); empty string reverts to the built-in placeholder.
    pub workflow_name: Option<String>,
}

pub type Shared = Arc<RwLock<Settings>>;

pub fn new(settings: Settings) -> Shared {
    Arc::new(RwLock::new(settings))
}
