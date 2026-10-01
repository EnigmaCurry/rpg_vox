//! Locating and downloading the sherpa-onnx model bundles. Layout
//! matches rpg_vox's Justfile, so its `models/` directory works as-is:
//!
//! ```text
//! <dir>/sense-voice/{model.int8.onnx,tokens.txt}
//! <dir>/streaming-zipformer/{encoder,decoder,joiner}.onnx + tokens.txt
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context as _, Result};

pub const SENSE_VOICE: &str = "sense-voice";
pub const ZIPFORMER: &str = "streaming-zipformer";

const RELEASES: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models";

/// Per-user data directory for downloaded models.
pub fn user_models_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/vox_scribe/models")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("vox_scribe/models")
    }
}

/// `--models-dir`, else `$VOX_SCRIBE_MODELS`, else the user data dir if
/// it has SenseVoice, else `./models` (an rpg_vox checkout).
pub fn resolve(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    if let Some(p) = std::env::var_os("VOX_SCRIBE_MODELS") {
        return PathBuf::from(p);
    }
    let user = user_models_dir();
    if user.join(SENSE_VOICE).join("model.int8.onnx").is_file() {
        return user;
    }
    let local = PathBuf::from("models");
    if local.join(SENSE_VOICE).join("model.int8.onnx").is_file() {
        return local;
    }
    user
}

pub fn has_zipformer(dir: &Path) -> bool {
    ["encoder.onnx", "decoder.onnx", "joiner.onnx", "tokens.txt"]
        .iter()
        .all(|f| dir.join(ZIPFORMER).join(f).is_file())
}

/// Download both bundles into `dir` with `curl` + `tar`, skipping any
/// that are already present.
pub fn download(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let sv = dir.join(SENSE_VOICE);
    if sv.join("model.int8.onnx").is_file() && sv.join("tokens.txt").is_file() {
        println!("SenseVoice already present in {}", sv.display());
    } else {
        fetch(
            "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17",
            &sv,
            &[
                ("model.int8.onnx", "model.int8.onnx"),
                ("tokens.txt", "tokens.txt"),
            ],
        )?;
    }
    if has_zipformer(dir) {
        println!(
            "Zipformer already present in {}",
            dir.join(ZIPFORMER).display()
        );
    } else {
        fetch(
            "sherpa-onnx-streaming-zipformer-en-20M-2023-02-17",
            &dir.join(ZIPFORMER),
            &[
                ("encoder-epoch-99-avg-1.int8.onnx", "encoder.onnx"),
                ("decoder-epoch-99-avg-1.onnx", "decoder.onnx"),
                ("joiner-epoch-99-avg-1.int8.onnx", "joiner.onnx"),
                ("tokens.txt", "tokens.txt"),
            ],
        )?;
    }
    Ok(())
}

fn fetch(stem: &str, dest: &Path, files: &[(&str, &str)]) -> Result<()> {
    let tmp = std::env::temp_dir().join(format!("vox_scribe-{stem}"));
    std::fs::create_dir_all(&tmp)?;
    let archive = tmp.join(format!("{stem}.tar.bz2"));
    let url = format!("{RELEASES}/{stem}.tar.bz2");
    println!("downloading {url}");
    let ok = Command::new("curl")
        .args(["-fL", "--progress-bar", "-o"])
        .arg(&archive)
        .arg(&url)
        .status()
        .context("run curl")?
        .success();
    if !ok {
        bail!("download failed: {url}");
    }
    let ok = Command::new("tar")
        .arg("-xjf")
        .arg(&archive)
        .arg("-C")
        .arg(&tmp)
        .status()?
        .success();
    if !ok {
        bail!("extract failed: {}", archive.display());
    }
    std::fs::create_dir_all(dest)?;
    for (from, to) in files {
        std::fs::copy(tmp.join(stem).join(from), dest.join(to))
            .with_context(|| format!("copy {from}"))?;
    }
    let _ = std::fs::remove_dir_all(&tmp);
    println!("installed {}", dest.display());
    Ok(())
}
