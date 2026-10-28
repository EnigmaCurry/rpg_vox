//! Build the Svelte SPA (crates/vox_room/web) and drop the output in
//! crates/vox_room/dist-ui/ so `rust-embed` picks it up at compile time.
//!
//! Requires `pnpm` on PATH.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    if std::env::var_os("DOCS_RS").is_some() {
        return;
    }
    // Opt-out for callers that already prebuilt dist-ui/ (CI, packaging).
    if std::env::var_os("VOX_ROOM_SKIP_UI_BUILD").is_some() {
        return;
    }

    let web = Path::new("web");
    println!("cargo:rerun-if-env-changed=VOX_ROOM_SKIP_UI_BUILD");
    println!("cargo:rerun-if-changed=web/package.json");
    println!("cargo:rerun-if-changed=web/pnpm-lock.yaml");
    println!("cargo:rerun-if-changed=web/vite.config.js");
    println!("cargo:rerun-if-changed=web/index.html");
    emit_rerun_for_tree(&web.join("src"));

    if !web.join("node_modules").exists() {
        run("pnpm", &["install"], web);
    }
    run("pnpm", &["run", "build"], web);
}

fn emit_rerun_for_tree(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p: PathBuf = entry.path();
        if p.is_dir() {
            emit_rerun_for_tree(&p);
        } else {
            println!("cargo:rerun-if-changed={}", p.display());
        }
    }
}

fn run(cmd: &str, args: &[&str], cwd: &Path) {
    let status = Command::new(cmd)
        .args(args)
        .current_dir(cwd)
        .status()
        .unwrap_or_else(|e| panic!("failed to run `{cmd}` (is it on PATH?): {e}"));
    if !status.success() {
        panic!("`{cmd} {}` failed with {status}", args.join(" "));
    }
}
