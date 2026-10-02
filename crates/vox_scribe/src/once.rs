//! One `--once` recorder at a time. The running recorder's pid lives in a
//! per-user file; launching another (or `vox_scribe stop-once`) sends it
//! SIGUSR1, which finishes and copies just like Enter.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::{Context as _, Result};

fn pid_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let dir = if cfg!(target_os = "macos") {
        home.join("Library/Caches/vox_scribe")
    } else {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cache"))
            .join("vox_scribe")
    };
    dir.join("once.pid")
}

/// The pid of a live `--once` recorder, if there is one.
fn running() -> Option<i32> {
    let pid: i32 = std::fs::read_to_string(pid_path())
        .ok()?
        .trim()
        .parse()
        .ok()?;
    if pid == std::process::id() as i32 {
        return None;
    }
    // Guard against a stale file whose pid now belongs to something else.
    let out = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .contains("vox_scribe")
        .then_some(pid)
}

/// Tell a running recorder to finish and copy. False if none is running.
pub fn signal_finish() -> bool {
    match running() {
        // SAFETY: plain kill(2) on a pid we just checked.
        Some(pid) => unsafe { libc::kill(pid, libc::SIGUSR1) == 0 },
        None => false,
    }
}

/// Removes the pid file when the recorder exits.
pub struct Guard(PathBuf);

impl Drop for Guard {
    fn drop(&mut self) {
        let ours = std::fs::read_to_string(&self.0)
            .is_ok_and(|s| s.trim() == std::process::id().to_string());
        if ours {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

/// Register as the running recorder; SIGUSR1 then sets `finish`.
pub fn claim(finish: Arc<AtomicBool>) -> Result<Guard> {
    let path = pid_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    std::fs::write(&path, std::process::id().to_string())
        .with_context(|| format!("write {}", path.display()))?;
    signal_hook::flag::register(signal_hook::consts::SIGUSR1, finish)
        .context("install SIGUSR1 handler")?;
    Ok(Guard(path))
}
