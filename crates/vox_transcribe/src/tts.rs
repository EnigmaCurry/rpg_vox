//! Text to speech with Kokoro 82M through sherpa-onnx, on the CPU.
//!
//! Expects sherpa-onnx's `kokoro-multi-lang-v1_0` bundle, trimmed to
//! what English needs:
//!
//! ```text
//! <dir>/{model.onnx,voices.bin,tokens.txt,lexicon-us-en.txt}
//! <dir>/espeak-ng-data/
//! ```
//!
//! The fp32 model, not the int8 one: on an M1 the int8 model ran at about
//! real time, the fp32 one about 3.5x faster than that.

use std::path::Path;

use anyhow::{bail, Context as _, Result};
use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig,
    OfflineTtsModelConfig,
};

/// Files [`Kokoro::load`] reads from its directory.
pub const KOKORO_FILES: [&str; 5] = [
    "model.onnx",
    "voices.bin",
    "tokens.txt",
    "lexicon-us-en.txt",
    "espeak-ng-data",
];

/// Kokoro v1.0's voices, by speaker id. The prefix is language and sex:
/// `af` American female, `bm` British male, and so on.
pub const VOICES: [&str; 54] = [
    "af_alloy",
    "af_aoede",
    "af_bella",
    "af_heart",
    "af_jessica",
    "af_kore",
    "af_nicole",
    "af_nova",
    "af_river",
    "af_sarah",
    "af_sky",
    "am_adam",
    "am_echo",
    "am_eric",
    "am_fenrir",
    "am_liam",
    "am_michael",
    "am_onyx",
    "am_puck",
    "am_santa",
    "bf_alice",
    "bf_emma",
    "bf_isabella",
    "bf_lily",
    "bm_daniel",
    "bm_fable",
    "bm_george",
    "bm_lewis",
    "ef_dora",
    "em_alex",
    "ff_siwis",
    "hf_alpha",
    "hf_beta",
    "hm_omega",
    "hm_psi",
    "if_sara",
    "im_nicola",
    "jf_alpha",
    "jf_gongitsune",
    "jf_nezumi",
    "jf_tebukuro",
    "jm_kumo",
    "pf_dora",
    "pm_alex",
    "pm_santa",
    "zf_xiaobei",
    "zf_xiaoni",
    "zf_xiaoxiao",
    "zf_xiaoyi",
    "zm_yunjian",
    "zm_yunxi",
    "zm_yunxia",
    "zm_yunyang",
    "em_santa",
];

/// English voices that sound clearly apart, handed out to speakers in
/// order: A, B, C, …
pub const CAST: [&str; 8] = [
    "af_heart",
    "am_michael",
    "bf_emma",
    "bm_george",
    "af_bella",
    "am_fenrir",
    "bf_isabella",
    "bm_lewis",
];

/// Speaker id of a voice name.
pub fn voice_id(name: &str) -> Option<i32> {
    VOICES.iter().position(|v| *v == name).map(|i| i as i32)
}

/// Whether `dir` holds everything [`Kokoro::load`] needs.
pub fn has_kokoro(dir: &Path) -> bool {
    KOKORO_FILES.iter().all(|f| dir.join(f).exists())
}

pub struct Kokoro {
    tts: OfflineTts,
    rate: u32,
}

impl Kokoro {
    pub fn load(dir: &Path, threads: i32) -> Result<Self> {
        if !has_kokoro(dir) {
            bail!("Kokoro model not found in {}", dir.display());
        }
        let path = |f: &str| Some(dir.join(f).to_string_lossy().into_owned());
        let config = OfflineTtsConfig {
            model: OfflineTtsModelConfig {
                kokoro: OfflineTtsKokoroModelConfig {
                    model: path("model.onnx"),
                    voices: path("voices.bin"),
                    tokens: path("tokens.txt"),
                    data_dir: path("espeak-ng-data"),
                    lexicon: path("lexicon-us-en.txt"),
                    lang: Some("en-us".into()),
                    ..Default::default()
                },
                num_threads: threads,
                debug: false,
                provider: Some("cpu".into()),
                ..Default::default()
            },
            max_num_sentences: 1,
            silence_scale: 0.2,
            ..Default::default()
        };
        let tts = OfflineTts::create(&config)
            .with_context(|| format!("load Kokoro from {}", dir.display()))?;
        let rate = tts.sample_rate() as u32;
        Ok(Self { tts, rate })
    }

    /// Output sample rate (24 kHz for Kokoro).
    pub fn sample_rate(&self) -> u32 {
        self.rate
    }

    /// `text` spoken by voice `sid` (see [`VOICES`]) as mono f32 at
    /// [`sample_rate`](Self::sample_rate).
    pub fn speak(&self, text: &str, sid: i32, speed: f32) -> Result<Vec<f32>> {
        let config = GenerationConfig {
            sid,
            speed,
            ..Default::default()
        };
        let audio = self
            .tts
            .generate_with_config(text, &config, None::<fn(&[f32], f32) -> bool>)
            .context("Kokoro produced no audio")?;
        Ok(audio.samples().to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cast_voices_exist() {
        assert_eq!(voice_id("af_heart"), Some(3));
        assert_eq!(voice_id("em_santa"), Some(53));
        assert!(CAST.iter().all(|v| voice_id(v).is_some()));
        assert_eq!(voice_id("nobody"), None);
    }
}
