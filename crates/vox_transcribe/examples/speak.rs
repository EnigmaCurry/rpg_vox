//! Speak a line with Kokoro and save it as a WAV, reporting speed.
//!
//! ```text
//! cargo run --release -p vox_transcribe --features sherpa-static --example speak -- \
//!     <kokoro dir> out.wav "Hello there." [voice]
//! ```

use std::io::Write as _;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context as _, Result};
use vox_transcribe::tts::{voice_id, Kokoro};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [dir, out, text, rest @ ..] = args.as_slice() else {
        anyhow::bail!("usage: speak <kokoro dir> <out.wav> <text> [voice]");
    };
    let voice = rest.first().map(String::as_str).unwrap_or("af_heart");
    let sid = voice_id(voice).with_context(|| format!("no voice {voice}"))?;

    let t = Instant::now();
    let tts = Kokoro::load(
        Path::new(dir),
        std::env::var("THREADS")
            .ok()
            .and_then(|t| t.parse().ok())
            .unwrap_or(4),
    )?;
    println!("loaded in {:.2?}", t.elapsed());

    let t = Instant::now();
    let samples = tts.speak(text, sid, 1.0)?;
    let took = t.elapsed().as_secs_f64();
    let secs = samples.len() as f64 / tts.sample_rate() as f64;
    println!(
        "{secs:.2}s of audio in {took:.2}s ({:.1}x real time)",
        secs / took
    );
    write_wav(Path::new(out), &samples, tts.sample_rate())
}

/// 16-bit mono PCM WAV.
fn write_wav(path: &Path, samples: &[f32], rate: u32) -> Result<()> {
    let data = (samples.len() * 2) as u32;
    let mut f = std::fs::File::create(path)?;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * 2).to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data.to_le_bytes())?;
    for s in samples {
        f.write_all(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())?;
    }
    Ok(())
}
