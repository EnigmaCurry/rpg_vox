//! Re-run the final diarization pass on a recording's words, for tuning:
//! diarize a 16-bit WAV (or reuse turns saved with SEGMENTS=file), then
//! relabel the words of an ASS file (as scribe wrote it) and print them
//! as `start_ms end_ms speaker word`.
//!
//! THREADS, NUM (speaker count), THRESHOLD, MIN_ON, MIN_OFF, SHIFT tune the
//! diarizer; MIN_UNIT_MS, SNAP, BONUS, TURN, ROUNDS, PROFILE_SEGMENTS the
//! refinement. RAW=1 prints the diarizer's labels without refinement.

use std::path::PathBuf;

use vox_transcribe::diarize::{relabel, relabel_refined, RefineConfig, Segment};
use vox_transcribe::sherpa::{Diarizer, DiarizerTuning, SpeakerEmbedder, SpeakerModels};
use vox_transcribe::{Clip, Paragraph, Stage, Transcript, Word};

fn env<T: std::str::FromStr>(k: &str) -> Option<T> {
    std::env::var(k).ok().and_then(|v| v.parse().ok())
}

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

/// One paragraph per ASS line, words timed by its `\k` tags.
fn transcript_from_ass(path: &str) -> Transcript {
    let clock = |x: &str| {
        let p: Vec<f64> = x.split(':').map(|v| v.parse().unwrap()).collect();
        ((p[0] * 3600.0 + p[1] * 60.0 + p[2]) * 1000.0) as u64
    };
    let mut t = Transcript::default();
    for (n, l) in std::fs::read_to_string(path).unwrap().lines().enumerate() {
        let Some(rest) = l.strip_prefix("Dialogue:") else {
            continue;
        };
        let f: Vec<&str> = rest.splitn(10, ',').collect();
        let mut at = clock(f[1].trim());
        let mut words = Vec::new();
        for part in f[9].split("{\\k").skip(1) {
            let (k, w) = part.split_once('}').unwrap();
            let d = k.parse::<u64>().unwrap() * 10;
            let w = w.replace("\\N", " ");
            if !w.trim().is_empty() {
                words.push(Word {
                    text: w.trim().into(),
                    start_ms: at,
                    end_ms: at + d,
                });
            }
            at += d;
        }
        if words.is_empty() {
            continue;
        }
        let text = words
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let (s, e) = (words[0].start_ms, words.last().unwrap().end_ms);
        let clip = Clip {
            id: format!("c{n}"),
            start_ms: s,
            duration_ms: Some(e - s),
            text: text.clone(),
            stage: Stage::Final,
            words: words.clone(),
            speaker: None,
        };
        let mut p = Paragraph {
            id: format!("p{n}"),
            start_ms: s,
            end_ms: e,
            text,
            clips: vec![clip],
            words,
            speaker: None,
            closed: true,
            hardened: true,
            pass3_inflight: false,
            pass4: None,
        };
        p.end_ms = e;
        t.paragraphs.push(p);
    }
    t
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (samples, rate) = read_wav(&a[1]);
    let t = transcript_from_ass(&a[2]);
    let models = SpeakerModels {
        segmentation: PathBuf::from(&a[3]),
        embedding: PathBuf::from(&a[4]),
        num_threads: env("THREADS").unwrap_or(4),
    };
    let segments: Vec<Segment> = match std::env::var("SEGMENTS") {
        Ok(p) if std::path::Path::new(&p).exists() => std::fs::read_to_string(&p)
            .unwrap()
            .lines()
            .map(|l| {
                let v: Vec<u64> = l.split_whitespace().map(|x| x.parse().unwrap()).collect();
                Segment {
                    start_ms: v[0],
                    end_ms: v[1],
                    speaker: v[2] as usize,
                }
            })
            .collect(),
        other => {
            let tuning = DiarizerTuning {
                threshold: env("THRESHOLD").unwrap_or(0.5),
                min_duration_on: env("MIN_ON").unwrap_or(0.3),
                min_duration_off: env("MIN_OFF").unwrap_or(0.5),
                window_shift_ratio: env("SHIFT").unwrap_or(0.1),
                ..Default::default()
            };
            let segs = Diarizer::open_tuned(&models, &tuning)
                .unwrap()
                .process(&samples, rate)
                .unwrap();
            if let Ok(p) = other {
                let s: String = segs
                    .iter()
                    .map(|s| format!("{} {} {}\n", s.start_ms, s.end_ms, s.speaker))
                    .collect();
                std::fs::write(p, s).unwrap();
            }
            segs
        }
    };
    // DUMP=file: one line per sentence, `start end` and its embedding.
    if let Ok(path) = std::env::var("DUMP") {
        let embedder = SpeakerEmbedder::open(&models).unwrap();
        let at = |ms: u64| ((ms * rate as u64 / 1000) as usize).min(samples.len());
        let words: Vec<&Word> = t.paragraphs.iter().flat_map(|p| &p.words).collect();
        let mut out = String::new();
        let mut start = 0;
        for i in 0..words.len() {
            let end = words[i]
                .text
                .trim_end_matches(['"', ')'])
                .ends_with(['.', '?', '!']);
            if end || i + 1 == words.len() {
                let (s, e) = (words[start].start_ms, words[i].end_ms);
                if let Some(v) = embedder.embed(&samples[at(s)..at(e)], rate) {
                    let v: Vec<String> = v.iter().map(|x| format!("{x:.5}")).collect();
                    out.push_str(&format!("{s} {e} {}\n", v.join(" ")));
                }
                start = i + 1;
            }
        }
        std::fs::write(path, out).unwrap();
        return;
    }
    let out = if env::<u8>("RAW") == Some(1) {
        relabel(&t, &segments)
    } else {
        let d = RefineConfig::default();
        let cfg = RefineConfig {
            min_unit_ms: env("MIN_UNIT_MS").unwrap_or(d.min_unit_ms),
            snap_words: env("SNAP").unwrap_or(d.snap_words),
            diarizer_bonus: env("BONUS").unwrap_or(d.diarizer_bonus),
            rounds: env("ROUNDS").unwrap_or(d.rounds),
            profile_segments: env("PROFILE_SEGMENTS").unwrap_or(d.profile_segments),
            max_speakers: env("NUM"),
            turn_bonus: env("TURN").unwrap_or(d.turn_bonus),
            ..d
        };
        let embedder = SpeakerEmbedder::open(&models).unwrap();
        let at = |ms: u64| ((ms * rate as u64 / 1000) as usize).min(samples.len());
        let mut embed = |s: u64, e: u64| embedder.embed(&samples[at(s)..at(e)], rate);
        relabel_refined(&t, &segments, &mut embed, &cfg)
    };
    for p in &out.paragraphs {
        for w in &p.words {
            println!(
                "{} {} {} {}",
                w.start_ms,
                w.end_ms,
                p.speaker.as_deref().unwrap_or("?"),
                w.text
            );
        }
    }
}
