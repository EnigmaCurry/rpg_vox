//! The AI's voice: a sentence of text in, 48 kHz mono samples out.
//!
//! * `qwen3`: a Qwen3-TTS server speaking vllm-omni's `/v1/audio/speech`
//!   (the `qwen3-tts/` stack in this repo, the same one `pi-tts.py` uses).
//! * `kokoro`: the local Kokoro model scribe can download, for when no
//!   server is around.
//! * `none`: silence as long as the sentence would take to read, so the
//!   room can be tried without any voice at all.

use std::io::Read as _;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use rubato::{
    Resampler as _, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

use crate::mixer::RATE;

pub trait Voice: Send {
    fn speak(&mut self, text: &str) -> Result<Vec<f32>>;
}

pub struct Qwen3 {
    url: String,
    voice: String,
    instruct: String,
    language: String,
    /// Bearer token, for a server that is not wide open.
    key: Option<String>,
    /// Named in the request only when the server serves more than one.
    model: Option<String>,
    http: ureq::Agent,
}

impl Qwen3 {
    pub fn new(
        url: &str,
        voice: &str,
        instruct: &str,
        language: &str,
        key: Option<String>,
        model: Option<String>,
    ) -> Self {
        Self {
            url: url.trim_end_matches('/').to_string(),
            voice: voice.to_string(),
            instruct: instruct.to_string(),
            language: language.to_string(),
            key,
            model,
            http: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(60))
                .build(),
        }
    }
}

impl Voice for Qwen3 {
    fn speak(&mut self, text: &str) -> Result<Vec<f32>> {
        let mut body = serde_json::json!({
            "input": text,
            "voice": self.voice,
            "language": self.language,
            "response_format": "wav",
        });
        if !self.instruct.is_empty() {
            body["instructions"] = self.instruct.clone().into();
        }
        if let Some(model) = &self.model {
            body["model"] = model.clone().into();
        }
        let url = format!("{}/v1/audio/speech", self.url);
        let mut req = self.http.post(&url);
        if let Some(key) = &self.key {
            req = req.set("Authorization", &format!("Bearer {key}"));
        }
        let resp = match req.send_json(body) {
            Ok(r) => r,
            Err(ureq::Error::Status(code, r)) => {
                let detail = r.into_string().unwrap_or_default();
                bail!(
                    "{url}: HTTP {code}: {}",
                    detail.chars().take(300).collect::<String>()
                );
            }
            Err(e) => return Err(e).with_context(|| format!("POST {url}")),
        };
        let mut wav = Vec::new();
        resp.into_reader()
            .read_to_end(&mut wav)
            .context("read TTS audio")?;
        let (samples, rate) = decode_wav(&wav)?;
        to_room_rate(&samples, rate)
    }
}

pub struct Kokoro {
    tts: vox_transcribe::tts::Kokoro,
    sid: i32,
}

impl Kokoro {
    pub fn load(dir: &Path, voice: &str) -> Result<Self> {
        let sid = vox_transcribe::tts::voice_id(voice)
            .with_context(|| format!("no Kokoro voice {voice:?}"))?;
        Ok(Self {
            tts: vox_transcribe::tts::Kokoro::load(dir, 2)?,
            sid,
        })
    }
}

impl Voice for Kokoro {
    fn speak(&mut self, text: &str) -> Result<Vec<f32>> {
        let samples = self.tts.speak(text, self.sid, 1.0)?;
        to_room_rate(&samples, self.tts.sample_rate())
    }
}

/// Silence for as long as reading the text aloud would take.
pub struct Silent;

/// Characters per second of speech, for [`Silent`].
const READING_CPS: f32 = 15.0;

impl Voice for Silent {
    fn speak(&mut self, text: &str) -> Result<Vec<f32>> {
        let secs = (text.chars().count() as f32 / READING_CPS).max(0.3);
        Ok(vec![0.0; (secs * RATE as f32) as usize])
    }
}

/// A WAV file as mono f32 and its sample rate.
fn decode_wav(bytes: &[u8]) -> Result<(Vec<f32>, u32)> {
    let mut r = hound::WavReader::new(std::io::Cursor::new(bytes)).context("TTS sent no WAV")?;
    let spec = r.spec();
    let channels = spec.channels.max(1) as usize;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().collect::<std::result::Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<std::result::Result<_, _>>()?
        }
    };
    let mono = interleaved
        .chunks(channels)
        .map(|c| c.iter().sum::<f32>() / channels as f32)
        .collect();
    Ok((mono, spec.sample_rate))
}

fn to_room_rate(samples: &[f32], rate: u32) -> Result<Vec<f32>> {
    if rate == RATE || samples.is_empty() {
        return Ok(samples.to_vec());
    }
    let params = SincInterpolationParameters {
        sinc_len: 128,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Cubic,
        oversampling_factor: 128,
        window: WindowFunction::BlackmanHarris2,
    };
    let ratio = RATE as f64 / rate as f64;
    let chunk = 1024;
    let mut rs = SincFixedIn::<f32>::new(ratio, 2.0, params, chunk, 1)?;
    let mut out = Vec::with_capacity((samples.len() as f64 * ratio) as usize + chunk * 4);
    let mut pos = 0;
    while pos + chunk <= samples.len() {
        out.extend_from_slice(&rs.process(&[&samples[pos..pos + chunk]], None)?[0]);
        pos += chunk;
    }
    // rubato refuses an empty partial chunk, so a length that is a whole
    // number of chunks skips straight to the flush.
    if pos < samples.len() {
        out.extend_from_slice(&rs.process_partial(Some(&[&samples[pos..]]), None)?[0]);
    }
    // Flush the filter's delay so the last word isn't clipped.
    out.extend_from_slice(&rs.process_partial::<&[f32]>(None, None)?[0]);
    let delay = (rs.output_delay()).min(out.len());
    out.drain(..delay);
    out.truncate((samples.len() as f64 * ratio).round() as usize);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resamples_tts_audio_to_the_room_rate() {
        let tone: Vec<f32> = (0..24_000)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / 24_000.0).sin() * 0.5)
            .collect();
        let out = to_room_rate(&tone, 24_000).unwrap();
        assert_eq!(out.len(), 48_000);
        let peak = out[1000..47_000].iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.5).abs() < 0.02, "peak {peak}");
    }

    #[test]
    fn resamples_any_length() {
        for n in [1, 100, 1023, 1024, 1025, 2048, 48_000] {
            let out = to_room_rate(&vec![0.1; n], 24_000).unwrap();
            assert_eq!(out.len(), n * 2, "{n} samples");
        }
    }

    #[test]
    fn reads_a_wav() {
        let mut bytes = Vec::new();
        {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: 24_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut w = hound::WavWriter::new(std::io::Cursor::new(&mut bytes), spec).unwrap();
            for _ in 0..100 {
                w.write_sample(16384i16).unwrap();
            }
            w.finalize().unwrap();
        }
        let (s, rate) = decode_wav(&bytes).unwrap();
        assert_eq!(rate, 24_000);
        assert_eq!(s.len(), 100);
        assert!((s[0] - 0.5).abs() < 1e-4);
    }

    #[test]
    fn silence_lasts_as_long_as_reading() {
        let s = Silent.speak("fifteen chars!!").unwrap();
        assert_eq!(s.len(), RATE as usize);
    }
}
