//! Locating and downloading the sherpa-onnx model bundles. Layout
//! matches rpg_vox's Justfile, so its `models/` directory works as-is:
//!
//! ```text
//! <dir>/sense-voice/{model.int8.onnx,tokens.txt}
//! <dir>/parakeet-tdt-0.6b-v2/{encoder,decoder,joiner}.int8.onnx + tokens.txt
//! <dir>/streaming-zipformer/{encoder,decoder,joiner}.onnx + tokens.txt
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context as _, Result};

pub const SENSE_VOICE: &str = "sense-voice";
pub const ZIPFORMER: &str = "streaming-zipformer";
pub const PARAKEET: &str = "parakeet-tdt-0.6b-v2";

/// The pass-2/3 recognizer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Offline {
    /// SenseVoice Small: multilingual, fast, ~230 MB.
    #[value(name = "sensevoice")]
    SenseVoice,
    /// NVIDIA Parakeet TDT 0.6B v2: English only, more accurate, ~660 MB.
    Parakeet,
}

impl Offline {
    pub fn present(self, dir: &Path) -> bool {
        match self {
            Offline::SenseVoice => has_sense_voice(dir),
            Offline::Parakeet => has_parakeet(dir),
        }
    }

    /// Approximate download for this model plus Zipformer.
    pub fn download_size(self) -> &'static str {
        match self {
            Offline::SenseVoice => "about 280 MB",
            Offline::Parakeet => "about 580 MB",
        }
    }
}

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
/// it has a pass-2 model, else `./models` (an rpg_vox checkout).
pub fn resolve(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    if let Some(p) = std::env::var_os("VOX_SCRIBE_MODELS") {
        return PathBuf::from(p);
    }
    let user = user_models_dir();
    if has_sense_voice(&user) || has_parakeet(&user) {
        return user;
    }
    let local = PathBuf::from("models");
    if has_sense_voice(&local) || has_parakeet(&local) {
        return local;
    }
    user
}

pub fn has_sense_voice(dir: &Path) -> bool {
    ["model.int8.onnx", "tokens.txt"]
        .iter()
        .all(|f| dir.join(SENSE_VOICE).join(f).is_file())
}

pub fn has_parakeet(dir: &Path) -> bool {
    [
        "encoder.int8.onnx",
        "decoder.int8.onnx",
        "joiner.int8.onnx",
        "tokens.txt",
    ]
    .iter()
    .all(|f| dir.join(PARAKEET).join(f).is_file())
}

pub fn has_zipformer(dir: &Path) -> bool {
    ["encoder.onnx", "decoder.onnx", "joiner.onnx", "tokens.txt"]
        .iter()
        .all(|f| dir.join(ZIPFORMER).join(f).is_file())
}

/// Download `offline` and Zipformer into `dir` with `curl` + `tar`,
/// skipping any that are already present.
pub fn download(dir: &Path, offline: Offline) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let shown = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    println!("models directory: {}", shown.display());
    let sv = dir.join(SENSE_VOICE);
    let pk = dir.join(PARAKEET);
    if offline == Offline::Parakeet {
        if has_parakeet(dir) {
            println!("Parakeet already present in {}", pk.display());
        } else {
            let files = [
                "encoder.int8.onnx",
                "decoder.int8.onnx",
                "joiner.int8.onnx",
                "tokens.txt",
            ]
            .map(|f| (f, f));
            fetch("sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8", &pk, &files)?;
        }
    } else if has_sense_voice(dir) {
        println!("SenseVoice already present in {}", sv.display());
    } else {
        fetch(
            "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17",
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
