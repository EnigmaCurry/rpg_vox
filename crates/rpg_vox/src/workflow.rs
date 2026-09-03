//! ComfyUI workflow loading + text substitution.
//!
//! A "workflow" is the JSON graph you'd normally paste into ComfyUI's `/prompt`
//! endpoint (top-level object keyed by node id, each value having `class_type`
//! and `inputs`). We hold one workflow at a time as the template for `/say`
//! requests and substitute the token `{{TEXT}}` with the utterance text.
//!
//! The default (built-in) workflow is a stub with a fake `TTS_PLACEHOLDER`
//! node — replace it by pointing `workflow_path` at a real exported workflow.

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use tracing::warn;

pub const TEXT_TOKEN: &str = "{{TEXT}}";

#[derive(Clone, Debug, Serialize, Default)]
pub struct WorkflowSummary {
    /// Human-readable origin: "builtin" or the file path we loaded from.
    pub source: String,
    pub node_count: usize,
    /// True if any string in the workflow contains `{{TEXT}}` — otherwise the
    /// utterance won't actually reach any node.
    pub has_text_placeholder: bool,
    /// Unique `class_type` values used in the workflow (sorted). Handy for
    /// verifying which custom nodes ComfyUI needs.
    pub node_classes: Vec<String>,
}

pub fn builtin_placeholder() -> (Value, WorkflowSummary) {
    let json = serde_json::json!({
        "1": {
            "class_type": "TTS_PLACEHOLDER",
            "inputs": { "text": TEXT_TOKEN }
        }
    });
    let summary = compute_summary(&json, "builtin".to_string());
    (json, summary)
}

pub fn load_from_path(path: &str) -> Result<(Value, WorkflowSummary)> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading workflow file {path}"))?;
    let json: Value = serde_json::from_str(&text)
        .with_context(|| format!("parsing workflow JSON at {path}"))?;
    let summary = compute_summary(&json, path.to_string());
    Ok((json, summary))
}

pub fn compute_summary(json: &Value, source: String) -> WorkflowSummary {
    let obj = json.as_object();
    let node_count = obj.map(|o| o.len()).unwrap_or(0);
    let mut classes: BTreeSet<String> = BTreeSet::new();
    if let Some(o) = obj {
        for node in o.values() {
            if let Some(ct) = node.get("class_type").and_then(|v| v.as_str()) {
                classes.insert(ct.to_string());
            }
        }
    }
    WorkflowSummary {
        source,
        node_count,
        has_text_placeholder: contains_text_token(json),
        node_classes: classes.into_iter().collect(),
    }
}

fn contains_text_token(v: &Value) -> bool {
    match v {
        Value::String(s) => s.contains(TEXT_TOKEN),
        Value::Array(a) => a.iter().any(contains_text_token),
        Value::Object(o) => o.values().any(contains_text_token),
        _ => false,
    }
}

/// Deep-clone the workflow with `{{TEXT}}` substituted everywhere it appears.
pub fn substitute_text(workflow: &Value, text: &str) -> Value {
    fn walk(v: &mut Value, text: &str) {
        match v {
            Value::String(s) if s.contains(TEXT_TOKEN) => {
                *s = s.replace(TEXT_TOKEN, text);
            }
            Value::Array(a) => a.iter_mut().for_each(|x| walk(x, text)),
            Value::Object(o) => o.values_mut().for_each(|x| walk(x, text)),
            _ => {}
        }
    }
    let mut cloned = workflow.clone();
    walk(&mut cloned, text);
    cloned
}

// -- Registry -----------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct RegistryEntry {
    pub name: String,
    #[allow(dead_code)] // kept for potential UI exposure / debugging
    pub path: PathBuf,
    pub json: Value,
    pub summary: WorkflowSummary,
}

/// Read-only catalog of workflows loaded from a directory at startup.
#[derive(Clone, Debug, Default)]
pub struct Registry {
    entries: BTreeMap<String, RegistryEntry>,
}

impl Registry {
    /// Scan a directory for `*.json`, load each. Invalid files are skipped
    /// with a warning; an unreadable directory is treated as empty.
    pub fn from_dir(dir: &Path) -> Self {
        let mut entries = BTreeMap::new();
        let read = match std::fs::read_dir(dir) {
            Ok(r) => r,
            Err(err) => {
                warn!(dir = %dir.display(), ?err, "workflows dir not readable; registry empty");
                return Self { entries };
            }
        };
        for e in read.flatten() {
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|s| s.to_str()).map(String::from) else {
                continue;
            };
            let Some(path_str) = path.to_str() else { continue };
            match load_from_path(path_str) {
                Ok((json, mut summary)) => {
                    // Present the friendlier name in the summary rather than
                    // the full path.
                    summary.source = name.clone();
                    entries.insert(
                        name.clone(),
                        RegistryEntry {
                            name,
                            path,
                            json,
                            summary,
                        },
                    );
                }
                Err(err) => {
                    warn!(path = %path.display(), ?err, "skipping invalid workflow");
                }
            }
        }
        Self { entries }
    }

    pub fn get(&self, name: &str) -> Option<&RegistryEntry> {
        self.entries.get(name)
    }

    pub fn first_name(&self) -> Option<&str> {
        self.entries.keys().next().map(|s| s.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> impl Iterator<Item = &RegistryEntry> {
        self.entries.values()
    }
}

/// Query ComfyUI `/object_info` and return the workflow's node classes that
/// are NOT registered on the server.
pub async fn missing_nodes(
    http: &reqwest::Client,
    comfyui_base: &str,
    workflow_classes: &[String],
) -> Result<Vec<String>> {
    let url = format!("{}/object_info", comfyui_base.trim_end_matches('/'));
    let resp = http.get(&url).send().await?.error_for_status()?;
    let info: Value = resp.json().await?;
    let obj = info
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("/object_info returned non-object"))?;
    Ok(workflow_classes
        .iter()
        .filter(|c| !obj.contains_key(c.as_str()))
        .cloned()
        .collect())
}
