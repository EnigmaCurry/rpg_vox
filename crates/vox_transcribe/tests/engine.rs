//! Engine behaviour with fake recognizers.

use std::sync::Arc;

use vox_transcribe::{
    Change, Engine, EngineConfig, Event, OfflineRecognizer, Stage, StreamingRecognizer,
};

const SR: u32 = 16_000;

/// Emits one "WORD" per 200 ms fed.
struct FakeStreaming {
    fed: usize,
}

impl StreamingRecognizer for FakeStreaming {
    fn reset(&mut self) {
        self.fed = 0;
    }
    fn feed(&mut self, samples: &[f32], _rate: u32) {
        self.fed += samples.len();
    }
    fn partial(&self) -> String {
        vec!["WORD"; self.fed / (SR as usize / 5)].join(" ")
    }
}

/// Sub-second buffers decode to nothing (noise), short ones to one
/// sentence, longer (joined) ones to the boundary-fixed version.
struct FakeOffline;

impl OfflineRecognizer for FakeOffline {
    fn transcribe(&self, samples: &[f32], rate: u32) -> anyhow::Result<String> {
        let secs = samples.len() as f32 / rate as f32;
        Ok(if secs < 1.5 {
            String::new()
        } else if secs < 3.0 {
            "Alpha beta.".into()
        } else {
            "Alpha beta, alpha beta.".into()
        })
    }
}

fn tone(ms: u32, amp: f32) -> Vec<f32> {
    let n = (SR * ms / 1000) as usize;
    (0..n).map(|i| amp * (i as f32 * 0.3).sin()).collect()
}

fn run(chunks: &[(u32, f32)]) -> (Vec<Event>, vox_transcribe::Transcript) {
    let mut cfg = EngineConfig::new(SR);
    cfg.partial_interval = std::time::Duration::ZERO;
    let engine = Engine::spawn(
        cfg,
        Some(Box::new(FakeStreaming { fed: 0 })),
        Arc::new(FakeOffline),
    );
    let events = engine.events().clone();
    for &(ms, amp) in chunks {
        for chunk in tone(ms, amp).chunks(SR as usize / 50) {
            engine.push(chunk);
        }
    }
    let t = engine.finish();
    (events.try_iter().collect(), t)
}

fn changes(events: &[Event]) -> Vec<Change> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Paragraph { change, .. } => Some(*change),
            _ => None,
        })
        .collect()
}

#[test]
fn three_passes_in_one_paragraph() {
    let (events, t) = run(&[
        (500, 0.0),
        (1200, 0.2),
        (800, 0.0),
        (1200, 0.2),
        (3000, 0.0),
    ]);
    let ch = changes(&events);
    assert_eq!(ch.first(), Some(&Change::Opened));
    assert!(ch.contains(&Change::Partial));
    assert!(ch.contains(&Change::Final));
    assert!(ch.contains(&Change::Revised), "{ch:?}");
    assert!(ch.contains(&Change::Hardened));
    assert_eq!(t.paragraphs.len(), 1);
    let p = &t.paragraphs[0];
    assert_eq!(p.clips.len(), 2);
    assert!(p.clips.iter().all(|c| c.stage == Stage::Revised));
    assert_eq!(p.text, "Alpha beta, alpha beta.");
    assert!(p.hardened);
}

#[test]
fn long_silence_opens_new_paragraph() {
    let (_, t) = run(&[(1200, 0.2), (4000, 0.0), (1200, 0.2), (1000, 0.0)]);
    assert_eq!(t.paragraphs.len(), 2);
    assert!(t.paragraphs.iter().all(|p| p.hardened));
    assert!(t
        .paragraphs
        .iter()
        .all(|p| p.clips[0].stage == Stage::Final));
}

#[test]
fn noise_blip_leaves_no_trace() {
    let (events, t) = run(&[(500, 0.0), (180, 0.2), (1000, 0.0)]);
    assert!(t.paragraphs.is_empty());
    let last = events
        .iter()
        .rev()
        .find(|e| !matches!(e, Event::Level { .. }));
    assert!(
        matches!(last, Some(Event::ParagraphRemoved { .. })),
        "{last:?}"
    );
}

#[test]
fn manual_mode_ignores_silence_until_break() {
    let mut cfg = EngineConfig::new(SR);
    cfg.paragraph.mode = vox_transcribe::ParagraphMode::Manual;
    let engine = Engine::spawn(
        cfg,
        Some(Box::new(FakeStreaming { fed: 0 })),
        Arc::new(FakeOffline),
    );
    let push = |ms, amp| {
        for chunk in tone(ms, amp).chunks(SR as usize / 50) {
            engine.push(chunk);
        }
    };
    push(1200, 0.2);
    push(4000, 0.0);
    push(1200, 0.2);
    push(4000, 0.0);
    engine.break_paragraph();
    push(1200, 0.2);
    push(1000, 0.0);
    let t = engine.finish();
    assert_eq!(t.paragraphs.len(), 2);
    assert_eq!(t.paragraphs[0].clips.len(), 2);
    assert_eq!(t.paragraphs[1].clips.len(), 1);
}
