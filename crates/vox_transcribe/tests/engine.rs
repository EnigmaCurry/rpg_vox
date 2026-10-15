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
    run_with(Arc::new(FakeOffline), chunks)
}

fn run_with(
    offline: Arc<dyn OfflineRecognizer>,
    chunks: &[(u32, f32)],
) -> (Vec<Event>, vox_transcribe::Transcript) {
    let mut cfg = EngineConfig::new(SR);
    cfg.partial_interval = std::time::Duration::ZERO;
    let engine = Engine::spawn(cfg, Some(Box::new(FakeStreaming { fed: 0 })), offline);
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

/// Punctuates the joined window without re-casing it, the way the real
/// recognizers do: a sentence break arrives mid-text, the word after it
/// lowercase.
struct UncasedOffline;

impl OfflineRecognizer for UncasedOffline {
    fn transcribe(&self, samples: &[f32], rate: u32) -> anyhow::Result<String> {
        let secs = samples.len() as f32 / rate as f32;
        Ok(if secs < 1.5 {
            String::new()
        } else if secs < 3.0 {
            "Alpha beta.".into()
        } else {
            "Alpha beta. alpha beta.".into()
        })
    }
}

#[test]
fn pass3_capitalizes_a_sentence_it_punctuated() {
    let (_, t) = run_with(
        Arc::new(UncasedOffline),
        &[
            (500, 0.0),
            (1200, 0.2),
            (800, 0.0),
            (1200, 0.2),
            (3000, 0.0),
        ],
    );
    let p = &t.paragraphs[0];
    assert_eq!(p.text, "Alpha beta. Alpha beta.");
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

struct FakeCorrector {
    fail: bool,
}

impl vox_transcribe::correct::Corrector for FakeCorrector {
    fn correct(
        &self,
        _text: &str,
        _ctx: &[String],
    ) -> anyhow::Result<Vec<vox_transcribe::correct::Edit>> {
        if self.fail {
            anyhow::bail!("endpoint down");
        }
        Ok(vec![vox_transcribe::correct::Edit {
            from: "alpha beta.".into(),
            to: "alpha gamma.".into(),
        }])
    }
}

fn run_with_corrector(fail: bool) -> (Vec<Event>, vox_transcribe::Transcript) {
    let mut cfg = EngineConfig::new(SR);
    cfg.partial_interval = std::time::Duration::ZERO;
    let engine = Engine::spawn_with(
        cfg,
        Some(Box::new(FakeStreaming { fed: 0 })),
        Arc::new(FakeOffline),
        Some(Arc::new(FakeCorrector { fail })),
    );
    let events = engine.events().clone();
    for (ms, amp) in [
        (500, 0.0),
        (1200, 0.2),
        (800, 0.0),
        (1200, 0.2),
        (3000, 0.0),
    ] {
        for chunk in tone(ms, amp).chunks(SR as usize / 50) {
            engine.push(chunk);
        }
    }
    let t = engine.finish();
    (events.try_iter().collect(), t)
}

#[test]
fn pass4_corrects_before_hardening() {
    let (events, t) = run_with_corrector(false);
    let ch = changes(&events);
    let corrected = ch
        .iter()
        .position(|c| *c == Change::Corrected)
        .expect("corrected");
    let hardened = ch
        .iter()
        .position(|c| *c == Change::Hardened)
        .expect("hardened");
    assert!(corrected < hardened);
    let p = &t.paragraphs[0];
    assert_eq!(p.text, "Alpha beta, alpha gamma.");
    assert!(matches!(
        p.pass4,
        Some(vox_transcribe::Pass4::Done { edits: 1, .. })
    ));
}

#[test]
fn pass4_failure_keeps_pass3_text() {
    let (_, t) = run_with_corrector(true);
    let p = &t.paragraphs[0];
    assert!(p.hardened);
    assert_eq!(p.text, "Alpha beta, alpha beta.");
    assert!(matches!(
        p.pass4,
        Some(vox_transcribe::Pass4::Failed { .. })
    ));
}

/// Labels utterances from a fixed list, in order.
struct FakeTagger(Vec<&'static str>);

impl vox_transcribe::SpeakerTagger for FakeTagger {
    fn identify(&mut self, _samples: &[f32], _rate: u32) -> Option<String> {
        (!self.0.is_empty()).then(|| self.0.remove(0).to_string())
    }
}

fn run_tagged(tags: Vec<&'static str>, chunks: &[(u32, f32)]) -> vox_transcribe::Transcript {
    let mut cfg = EngineConfig::new(SR);
    cfg.partial_interval = std::time::Duration::ZERO;
    let engine = Engine::spawn_full(
        cfg,
        Some(Box::new(FakeStreaming { fed: 0 })),
        Arc::new(FakeOffline),
        None,
        Some(Box::new(FakeTagger(tags))),
    );
    for &(ms, amp) in chunks {
        for chunk in tone(ms, amp).chunks(SR as usize / 50) {
            engine.push(chunk);
        }
    }
    engine.finish()
}

const TWO_CLOSE_UTTERANCES: &[(u32, f32)] = &[
    (500, 0.0),
    (1200, 0.2),
    (800, 0.0),
    (1200, 0.2),
    (3000, 0.0),
];

/// Decodes like [`FakeOffline`] but slowly, so pass 2 falls behind the
/// VAD the way it does on a file pushed faster than real time.
struct SlowOffline;

impl OfflineRecognizer for SlowOffline {
    fn transcribe(&self, samples: &[f32], rate: u32) -> anyhow::Result<String> {
        std::thread::sleep(std::time::Duration::from_millis(80));
        FakeOffline.transcribe(samples, rate)
    }
}

/// A cap has to hold while pass 2 lags: a clip joins its paragraph when
/// pass 1 opens it, so waiting for the finalize to check would let a
/// paragraph swallow every utterance the decoder is still behind on.
#[test]
fn word_caps_hold_while_pass2_lags() {
    let utterances: &[(u32, f32)] = &[
        (500, 0.0),
        (1200, 0.2),
        (800, 0.0),
        (1200, 0.2),
        (800, 0.0),
        (1200, 0.2),
        (3000, 0.0),
    ];
    let run_capped = |soft: usize, hard: usize| {
        let mut cfg = EngineConfig::new(SR);
        cfg.partial_interval = std::time::Duration::ZERO;
        cfg.paragraph.soft_max_words = soft;
        cfg.paragraph.hard_max_words = hard;
        let engine = Engine::spawn(
            cfg,
            Some(Box::new(FakeStreaming { fed: 0 })),
            Arc::new(SlowOffline),
        );
        for &(ms, amp) in utterances {
            for chunk in tone(ms, amp).chunks(SR as usize / 50) {
                engine.push(chunk);
            }
        }
        engine.finish()
    };
    // Uncapped the three utterances are 800 ms apart, so they share a
    // paragraph however far behind pass 2 is.
    let t = run_capped(usize::MAX, usize::MAX);
    assert_eq!(t.paragraphs.len(), 1, "{t:#?}");
    // Capped, each utterance is past the cap by itself, so the next one
    // opens a paragraph — and the one it leaves is closed.
    for (soft, hard) in [(2, usize::MAX), (usize::MAX, 2)] {
        let t = run_capped(soft, hard);
        assert_eq!(t.paragraphs.len(), 3, "soft={soft} hard={hard}: {t:#?}");
        assert!(t.paragraphs.iter().all(|p| p.hardened), "{t:#?}");
    }
}

/// The word caps `--paragraph-soft-words` / `--paragraph-hard-words`
/// set. Each utterance here decodes to the two words "Alpha beta", and
/// the 800 ms between them is short enough that the gap alone would
/// keep them in one paragraph.
#[test]
fn word_caps_close_paragraphs() {
    let run_capped = |soft: usize, hard: usize| {
        let mut cfg = EngineConfig::new(SR);
        cfg.partial_interval = std::time::Duration::ZERO;
        cfg.paragraph.soft_max_words = soft;
        cfg.paragraph.hard_max_words = hard;
        let engine = Engine::spawn(
            cfg,
            Some(Box::new(FakeStreaming { fed: 0 })),
            Arc::new(FakeOffline),
        );
        for &(ms, amp) in TWO_CLOSE_UTTERANCES {
            for chunk in tone(ms, amp).chunks(SR as usize / 50) {
                engine.push(chunk);
            }
        }
        engine.finish()
    };
    // Defaults: one paragraph, two clips, four words.
    let t = run_capped(100, 200);
    assert_eq!(t.paragraphs.len(), 1, "{t:#?}");
    // A two-word cap is reached by the first clip, so the second opens
    // a paragraph of its own. Both caps do it on an utterance that
    // ended on silence.
    for (soft, hard) in [(2, 200), (100, 2)] {
        let t = run_capped(soft, hard);
        assert_eq!(t.paragraphs.len(), 2, "soft={soft} hard={hard}: {t:#?}");
        assert!(t.paragraphs.iter().all(|p| p.clips.len() == 1), "{t:#?}");
    }
    // Off, the way --chat turns them off.
    let t = run_capped(usize::MAX, usize::MAX);
    assert_eq!(t.paragraphs.len(), 1, "{t:#?}");
}

#[test]
fn speaker_change_opens_new_paragraph() {
    let t = run_tagged(vec!["A", "B"], TWO_CLOSE_UTTERANCES);
    assert_eq!(t.paragraphs.len(), 2, "{t:#?}");
    assert_eq!(t.paragraphs[0].speaker.as_deref(), Some("A"));
    assert_eq!(t.paragraphs[1].speaker.as_deref(), Some("B"));
    assert!(t
        .paragraphs
        .iter()
        .all(|p| p.hardened && p.clips.len() == 1));
}

#[test]
fn same_speaker_stays_in_one_paragraph() {
    let t = run_tagged(vec!["A", "A"], TWO_CLOSE_UTTERANCES);
    assert_eq!(t.paragraphs.len(), 1);
    assert_eq!(t.paragraphs[0].speaker.as_deref(), Some("A"));
    assert_eq!(t.paragraphs[0].clips.len(), 2);
}

/// Emits one "WORD" per 200 ms fed during the first second of the
/// stream, then nothing: speech that trails off into steady music.
struct TrailingOff {
    /// Samples fed in all, and when the current utterance began.
    total: usize,
    start: usize,
}

impl StreamingRecognizer for TrailingOff {
    fn reset(&mut self) {
        self.start = self.total;
    }
    fn feed(&mut self, samples: &[f32], _rate: u32) {
        self.total += samples.len();
    }
    fn partial(&self) -> String {
        let speech = |n: usize| n.min(SR as usize);
        let n = (speech(self.total) - speech(self.start)) / (SR as usize / 5);
        vec!["WORD"; n].join(" ")
    }
}

#[test]
fn loud_utterance_without_new_words_is_cut() {
    let mut cfg = EngineConfig::new(SR);
    cfg.partial_interval = std::time::Duration::ZERO;
    // Constant loudness for 8 s: without the cut the VAD would hold it
    // open until 12 s (and only the flush at the end would close it).
    cfg.vad.noise_margin = 0.0;
    let engine = Engine::spawn(
        cfg,
        Some(Box::new(TrailingOff { total: 0, start: 0 })),
        Arc::new(FakeOffline),
    );
    let events = engine.events().clone();
    for chunk in tone(8_000, 0.2).chunks(SR as usize / 50) {
        engine.push(chunk);
    }
    let t = engine.finish();
    let _ = events.try_iter().count();
    // The words stop after 1 s, so the first utterance is cut 1.5 s
    // later; the rest (no words at all) is cut into pieces as long.
    let clips: Vec<_> = t.paragraphs.iter().flat_map(|p| &p.clips).collect();
    let durations: Vec<_> = clips.iter().filter_map(|c| c.duration_ms).collect();
    assert!(durations.len() >= 3, "{clips:?}");
    assert!((2_000..3_500).contains(&durations[0]), "{durations:?}");
    assert!(durations.iter().all(|&d| d < 3_500), "{durations:?}");
}
