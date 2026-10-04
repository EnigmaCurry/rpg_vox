//! Diarize a 16-bit PCM WAV and print the speaker turns, for tuning.
//!
//! cargo run --release -p vox_transcribe --features sherpa-static \
//!   --example diarize_file -- FILE.wav SEG.onnx EMB.onnx [N|auto] [THRESHOLD]

use std::path::PathBuf;

use vox_transcribe::sherpa::{Diarizer, DiarizerTuning, SpeakerModels};

fn read_wav(path: &str) -> (Vec<f32>, u32) {
    let b = std::fs::read(path).expect("read wav");
    let u16le = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let u32le = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    let (mut i, mut rate, mut ch, mut data) = (12, 0, 1, (0, 0));
    while i + 8 <= b.len() {
        let (id, len) = (&b[i..i + 4], u32le(i + 4) as usize);
        if id == b"fmt " {
            ch = u16le(i + 10) as usize;
            rate = u32le(i + 12);
        } else if id == b"data" {
            data = (i + 8, len.min(b.len() - i - 8));
        }
        i += 8 + len + (len & 1);
    }
    let pcm: Vec<f32> = b[data.0..data.0 + data.1]
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

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (samples, rate) = read_wav(&a[1]);
    let models = SpeakerModels {
        segmentation: PathBuf::from(&a[2]),
        embedding: PathBuf::from(&a[3]),
        num_threads: 4,
    };
    let tuning = DiarizerTuning {
        num_speakers: a.get(4).and_then(|n| n.parse().ok()),
        threshold: a.get(5).and_then(|t| t.parse().ok()).unwrap_or(0.5),
        ..Default::default()
    };
    let d = Diarizer::open_tuned(&models, &tuning).expect("open diarizer");
    for s in d.process(&samples, rate).expect("diarize") {
        println!("{} {} {}", s.start_ms, s.end_ms, s.speaker);
    }
}
