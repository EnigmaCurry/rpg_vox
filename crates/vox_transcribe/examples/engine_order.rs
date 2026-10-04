//! Feed a 16-bit WAV through the engine with live speaker labels, at
//! real-time pace (or N× with SPEED=n), and report any point where the
//! transcript's paragraphs or clips are out of time order.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use vox_transcribe::sherpa::{
    EmbeddingTagger, Parakeet, ParakeetConfig, SpeakerModels, Zipformer, ZipformerConfig,
};
use vox_transcribe::speaker::ClusterConfig;
use vox_transcribe::{Engine, EngineConfig, Event, Transcript};

fn read_wav(path: &str) -> (Vec<f32>, u32) {
    let b = std::fs::read(path).expect("read wav");
    let u32le = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    let (mut i, mut rate, mut data) = (12, 0, (0, 0));
    while i + 8 <= b.len() {
        let (id, len) = (&b[i..i + 4], u32le(i + 4) as usize);
        if id == b"fmt " {
            rate = u32le(i + 12);
        } else if id == b"data" {
            data = (i + 8, len.min(b.len() - i - 8));
        }
        i += 8 + len + (len & 1);
    }
    let pcm = b[data.0..data.0 + data.1]
        .chunks_exact(2)
        .map(|s| i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0)
        .collect();
    (pcm, rate)
}

fn check(t: &Transcript, when: &str) {
    // A streaming clip older than a finalized one.
    let clips: Vec<_> = t.paragraphs.iter().flat_map(|p| &p.clips).collect();
    if let Some(oldest_partial) = clips
        .iter()
        .filter(|c| c.is_partial())
        .map(|c| c.start_ms)
        .min()
    {
        if let Some(newer_final) = clips
            .iter()
            .filter(|c| !c.is_partial() && c.start_ms > oldest_partial)
            .map(|c| c.start_ms)
            .max()
        {
            println!(
                "{when}: partial clip at {oldest_partial} older than final clip at {newer_final}"
            );
        }
    }
    let mut last = 0;
    for p in &t.paragraphs {
        if p.start_ms < last {
            println!(
                "{when}: paragraph at {} after one ending {}",
                p.start_ms, last
            );
        }
        let mut c_last = 0;
        for c in &p.clips {
            if c.start_ms < c_last {
                println!(
                    "{when}: clip at {} after {} in paragraph {}",
                    c.start_ms, c_last, p.start_ms
                );
            }
            c_last = c.start_ms;
        }
        last = p.start_ms;
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (samples, rate) = read_wav(&a[1]);
    let models = Path::new(&a[2]);
    let speed: f64 = std::env::var("SPEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1.0);
    let offline = Parakeet::open(&ParakeetConfig::from_dir(
        &models.join("parakeet-tdt-0.6b-v2"),
    ))
    .unwrap();
    let streaming = Zipformer::open(&ZipformerConfig::from_dir(
        &models.join("streaming-zipformer"),
    ))
    .unwrap();
    let tagger = EmbeddingTagger::open(
        &SpeakerModels::from_dir(&models.join("speaker-diarization")),
        ClusterConfig::default(),
    )
    .unwrap();
    let engine = Engine::spawn_full(
        EngineConfig::new(rate),
        Some(Box::new(streaming.session())),
        Arc::new(offline),
        None,
        Some(Box::new(tagger)),
    );
    let events = engine.events().clone();
    let mut t = Transcript::default();
    let chunk = rate as usize / 50;
    let start = Instant::now();
    for (i, c) in samples.chunks(chunk).enumerate() {
        engine.push(c);
        let due = Duration::from_secs_f64((i + 1) as f64 * 0.02 / speed);
        if let Some(wait) = due.checked_sub(start.elapsed()) {
            std::thread::sleep(wait);
        }
        for ev in events.try_iter() {
            match ev {
                Event::Paragraph { paragraph, change } => {
                    t.upsert(&paragraph);
                    check(&t, &format!("{:?} at {:.1}s", change, (i as f64) * 0.02));
                }
                Event::ParagraphRemoved { id } => t.remove(&id),
                _ => {}
            }
        }
    }
    let fin = engine.finish();
    check(&fin, "final");
    for p in &fin.paragraphs {
        println!(
            "[{:>6}] {:?} {}",
            p.start_ms,
            p.speaker,
            p.text.chars().take(60).collect::<String>()
        );
    }
}
