//! Locating and downloading the sherpa-onnx model bundles. Layout
//! matches rpg_vox's Justfile, so its `models/` directory works as-is:
//!
//! ```text
//! <dir>/sense-voice/{model.int8.onnx,tokens.txt}
//! <dir>/parakeet-tdt-0.6b-v2/{encoder,decoder,joiner}.int8.onnx + tokens.txt
//! <dir>/streaming-zipformer/{encoder,decoder,joiner}.onnx + tokens.txt
//! <dir>/speaker-diarization/{segmentation,embedding-eres2netv2}.onnx   (--diarize)
//! <dir>/kokoro/{model.onnx,voices.bin,…}        (--play NAME.md, --tts kokoro)
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context as _, Result};

pub const SENSE_VOICE: &str = "sense-voice";
pub const ZIPFORMER: &str = "streaming-zipformer";
pub const PARAKEET: &str = "parakeet-tdt-0.6b-v2";
pub const SPEAKERS: &str = "speaker-diarization";
pub const KOKORO: &str = "kokoro";

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

    /// As `--model` spells it.
    pub fn name(self) -> &'static str {
        match self {
            Offline::SenseVoice => "sensevoice",
            Offline::Parakeet => "parakeet",
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

const RELEASES: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download";
const ASR: &str = "asr-models";
/// pyannote segmentation 3.0, ~6 MB.
const SEGMENTATION: &str = "sherpa-onnx-pyannote-segmentation-3-0";
/// 3D-Speaker ERes2NetV2 (200k speakers, many languages), ~71 MB. On a
/// real two-man podcast it separated the voices where WeSpeaker
/// ResNet34 split one of them into extra speakers, and it did better on
/// the synthetic tests too. The release tag's misspelling is upstream's.
const EMBEDDING_URL: &str =
    "speaker-recongition-models/3dspeaker_speech_eres2netv2_sv_zh-cn_16k-common.onnx";
/// File names in the speakers dir; the embedding file is named after its
/// model so an install with the old one fetches the new one.
pub const SEGMENTATION_FILE: &str = "segmentation.onnx";
pub const EMBEDDING_FILE: &str = "embedding-eres2netv2.onnx";

/// Per-user data directory for downloaded models.
pub fn user_models_dir() -> PathBuf {
    // Hundreds of megabytes that belong to this machine: LOCALAPPDATA,
    // not the roaming APPDATA that follows the user between them.
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("USERPROFILE")
                    .map(PathBuf::from)
                    .unwrap_or_default()
                    .join("AppData")
                    .join("Local")
            })
            .join("vox_scribe")
            .join("models")
    }
    #[cfg(not(target_os = "windows"))]
    {
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

pub fn has_speakers(dir: &Path) -> bool {
    [SEGMENTATION_FILE, EMBEDDING_FILE]
        .iter()
        .all(|f| dir.join(SPEAKERS).join(f).is_file())
}

/// Download the --diarize models into `dir`, unless already present.
pub fn download_speakers(dir: &Path) -> Result<()> {
    let dest = dir.join(SPEAKERS);
    if has_speakers(dir) {
        println!("speaker models already present in {}", dest.display());
        return Ok(());
    }
    std::fs::create_dir_all(&dest).with_context(|| format!("create {}", dest.display()))?;
    if !dest.join(SEGMENTATION_FILE).is_file() {
        fetch_in(
            "speaker-segmentation-models",
            SEGMENTATION,
            &dest,
            &[("model.onnx", SEGMENTATION_FILE)],
        )?;
    }
    let to = dest.join(EMBEDDING_FILE);
    if !to.is_file() {
        let part = dest.join(format!("{EMBEDDING_FILE}.part"));
        curl(&format!("{RELEASES}/{EMBEDDING_URL}"), &part)?;
        std::fs::rename(&part, &to)?;
        println!("installed {}", to.display());
    }
    // The previous embedding model, superseded.
    let _ = std::fs::remove_file(dest.join("embedding.onnx"));
    Ok(())
}

pub fn kokoro_dir(dir: &Path) -> PathBuf {
    dir.join(KOKORO)
}

pub fn has_kokoro(dir: &Path) -> bool {
    vox_transcribe::tts::has_kokoro(&kokoro_dir(dir))
}

/// Download the Kokoro voice model (English parts only) into `dir`.
pub fn download_kokoro(dir: &Path) -> Result<()> {
    if has_kokoro(dir) {
        println!("Kokoro already present in {}", kokoro_dir(dir).display());
        return Ok(());
    }
    let files = vox_transcribe::tts::KOKORO_FILES.map(|f| (f, f));
    fetch_in(
        "tts-models",
        "kokoro-multi-lang-v1_0",
        &kokoro_dir(dir),
        &files,
    )
}

pub fn has_zipformer(dir: &Path) -> bool {
    ["encoder.onnx", "decoder.onnx", "joiner.onnx", "tokens.txt"]
        .iter()
        .all(|f| dir.join(ZIPFORMER).join(f).is_file())
}

/// Download `offline` and Zipformer into `dir` with `curl`, skipping
/// any that are already present.
pub fn download(dir: &Path, offline: Offline) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let shown = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    // Windows canonicalizes to the `\\?\C:\…` extended-length form.
    let shown = shown
        .to_str()
        .and_then(|s| s.strip_prefix(r"\\?\"))
        .map(PathBuf::from)
        .unwrap_or(shown);
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
    fetch_in(ASR, stem, dest, files)
}

fn curl(url: &str, to: &Path) -> Result<()> {
    println!("downloading {url}");
    let ok = Command::new("curl")
        .args(["-fL", "--progress-bar", "-o"])
        .arg(to)
        .arg(url)
        .status()
        .context("run curl")?
        .success();
    if !ok {
        bail!("download failed: {url}");
    }
    Ok(())
}

/// Fetch `<release>/<stem>.tar.bz2` and copy `files` out of it.
fn fetch_in(release: &str, stem: &str, dest: &Path, files: &[(&str, &str)]) -> Result<()> {
    let tmp = std::env::temp_dir().join(format!("vox_scribe-{stem}"));
    std::fs::create_dir_all(&tmp)?;
    let archive = tmp.join(format!("{stem}.tar.bz2"));
    curl(&format!("{RELEASES}/{release}/{stem}.tar.bz2"), &archive)?;
    untar_bz2(&archive, &tmp)?;
    std::fs::create_dir_all(dest)?;
    for (from, to) in files {
        copy_all(&tmp.join(stem).join(from), &dest.join(to))
            .with_context(|| format!("copy {from}"))?;
    }
    let _ = std::fs::remove_dir_all(&tmp);
    println!("installed {}", dest.display());
    Ok(())
}

/// Unpack a `.tar.bz2`. Done in process rather than by running `tar`,
/// which isn't on every machine (and whose bzip2 support, where it is,
/// can be a separate matter).
fn untar_bz2(archive: &Path, into: &Path) -> Result<()> {
    // `unpack_in` canonicalizes `into` to keep entries inside it, so it
    // has to be there first.
    std::fs::create_dir_all(into).with_context(|| format!("create {}", into.display()))?;
    let file =
        std::fs::File::open(archive).with_context(|| format!("open {}", archive.display()))?;
    let reader = bzip2::read::BzDecoder::new(std::io::BufReader::new(file));
    let mut tar = tar::Archive::new(reader);
    let what = || format!("extract {}", archive.display());
    for entry in tar.entries().with_context(what)? {
        let mut entry = entry.with_context(what)?;
        // The bundles hold plain files and directories. Skipping
        // everything else means a stray symlink can't fail the install
        // on Windows, where making one needs a privilege.
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            continue;
        }
        // `unpack_in` refuses to write outside `into`.
        entry.unpack_in(into).with_context(what)?;
    }
    Ok(())
}

/// Copy a file, or a directory and everything in it.
fn copy_all(from: &Path, to: &Path) -> Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_all(&entry.path(), &to.join(entry.file_name()))?;
        }
    } else {
        std::fs::copy(from, to)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bundle like the ones on the sherpa-onnx releases page: a
    /// directory named after the archive, with a stray symlink in it.
    fn bundle(path: &Path) {
        let bz = bzip2::write::BzEncoder::new(
            std::fs::File::create(path).expect("create archive"),
            bzip2::Compression::fast(),
        );
        let mut tar = tar::Builder::new(bz);
        let mut file = |name: &str, body: &[u8]| {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, name, body).expect("append");
        };
        file("bundle/tokens.txt", b"a b c\n");
        file("bundle/test_wavs/one.wav", b"RIFF");
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_size(0);
        link.set_cksum();
        tar.append_link(&mut link, "bundle/latest.onnx", "tokens.txt")
            .expect("append link");
        tar.into_inner()
            .expect("finish tar")
            .finish()
            .expect("finish bzip2");
    }

    #[test]
    fn untar_bz2_unpacks_files_and_skips_links() {
        let tmp = std::env::temp_dir().join("vox_scribe-untar-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).expect("create tmp");
        let archive = tmp.join("bundle.tar.bz2");
        bundle(&archive);

        let into = tmp.join("out");
        untar_bz2(&archive, &into).expect("untar");

        assert_eq!(
            std::fs::read_to_string(into.join("bundle/tokens.txt")).expect("tokens.txt"),
            "a b c\n"
        );
        assert!(into.join("bundle/test_wavs/one.wav").is_file());
        // Skipped, not followed: nothing of it is written at all.
        assert!(!into.join("bundle/latest.onnx").exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
