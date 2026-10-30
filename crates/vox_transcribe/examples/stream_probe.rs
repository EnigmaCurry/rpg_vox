//! Feed a 16-bit PCM WAV to a streaming transducer bundle and print the
//! partial text as it grows, then the real time factor. For comparing
//! streaming models (latency, granularity, cost) before picking one:
//!
//!   cargo run --release -p vox_transcribe --features sherpa-static \
//!     --example stream_probe -- MODEL_DIR FILE.wav
//!
//! MODEL_DIR holds `encoder.onnx`, `decoder.onnx`, `joiner.onnx` and
//! `tokens.txt`, the layout `scribe download-models` installs.
//!
//! SLICE=ms sets how much audio is pushed at a time (default 100, as the
//! room does); THREADS=n the threads per model (default 2, like scribe).
//!
//! The partial is printed only when it changes, so the gaps between lines
//! are the model's update granularity -- which is what you are usually
//! here to compare.

use std::path::PathBuf;

use vox_transcribe::recognizer::StreamingRecognizer;
use vox_transcribe::sherpa::{Zipformer, ZipformerConfig};

fn env<T: std::str::FromStr>(k: &str) -> Option<T> {
    std::env::var(k).ok().and_then(|v| v.parse().ok())
}

fn read_wav(path: &str) -> (Vec<f32>, u32) {
    let b = std::fs::read(path).expect("read wav");
    let u16le = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let u32le = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    let (mut i, mut rate, mut ch, mut data) = (12, 0, 1, (0, 0));
    while i + 8 <= b.len() {
        let (id, len) = (&b[i..i + 4], u32le(i + 4) as usize);
        if id == b"fmt " {
            ch = u16le(i + 10).max(1) as usize;
            rate = u32le(i + 12);
        } else if id == b"data" {
            data = (i + 8, len.min(b.len() - i - 8));
        }
        i += 8 + len + (len & 1);
    }
    // Mono, as every recognizer here wants.
    let pcm = b[data.0..data.0 + data.1]
        .chunks_exact(2 * ch)
        .map(|f| {
            f.chunks_exact(2)
                .map(|s| i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0)
                .sum::<f32>()
                / ch as f32
        })
        .collect();
    (pcm, rate)
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let (Some(dir), Some(wav)) = (args.next(), args.next()) else {
        eprintln!("usage: stream_probe MODEL_DIR FILE.wav");
        std::process::exit(2);
    };
    let slice_ms: u64 = env("SLICE").unwrap_or(100);
    let mut cfg = ZipformerConfig::from_dir(&PathBuf::from(&dir));
    if let Some(n) = env("THREADS") {
        cfg.num_threads = n;
    }
    let model = Zipformer::open(&cfg)?;
    let mut session = model.session();

    let (samples, rate) = read_wav(&wav);
    let secs = samples.len() as f32 / rate as f32;
    println!("{wav}: {rate} Hz, {} samples ({secs:.2}s)", samples.len());
    println!("{dir}: slice {slice_ms} ms, {} threads\n", cfg.num_threads);

    // Push in slices, like a mic does, so the partials appear as they would
    // live rather than all at once.
    let slice = (rate as u64 * slice_ms / 1000).max(1) as usize;
    let started = std::time::Instant::now();
    let mut shown = String::new();
    for (i, chunk) in samples.chunks(slice).enumerate() {
        session.feed(chunk, rate);
        let partial = session.partial();
        if partial != shown {
            println!("[{:>6} ms] {partial}", (i as u64 + 1) * slice_ms);
            shown = partial;
        }
    }
    let took = started.elapsed().as_secs_f32();
    println!("\n{shown}");
    println!(
        "\ndecoded {secs:.2}s of audio in {took:.2}s -- RTF {:.3}",
        took / secs
    );
    Ok(())
}
