//! Several audio sources at once (`-d mic1 -d mic2 -a Discord`), one
//! speaker each: every source gets its own engine, so overlapping speech
//! is transcribed channel by channel rather than diarized from a mix.
//! Their events merge into one stream with each paragraph labelled by
//! its source (A, B, … in command-line order), and --record mixes the
//! sources into one .opus.

use std::collections::VecDeque;
use std::path::Path;
use std::thread::JoinHandle;

use anyhow::Result;
use crossbeam_channel::{unbounded, Receiver, Select, Sender};
use vox_audio::opus_file::{OpusWriter, RATE};
use vox_audio::resample::Linear;
use vox_transcribe::speaker::label;
use vox_transcribe::{Engine, Event, Paragraph, ParagraphMode, Pusher, Transcript};

/// One engine per source, driven as one.
pub struct Engines {
    engines: Vec<Engine>,
    events: Receiver<Event>,
    merger: Option<JoinHandle<()>>,
}

impl Engines {
    /// A single engine is passed through untouched (its speakers, if
    /// any, come from --diarize); several are labelled by source.
    pub fn new(engines: Vec<Engine>) -> Self {
        if engines.len() == 1 {
            let events = engines[0].events().clone();
            return Self {
                engines,
                events,
                merger: None,
            };
        }
        let (tx, events) = unbounded();
        let rxs: Vec<Receiver<Event>> = engines.iter().map(|e| e.events().clone()).collect();
        let merger = std::thread::Builder::new()
            .name("vox-merge".into())
            .spawn(move || merge(rxs, tx))
            .expect("spawn event merger");
        Self {
            engines,
            events,
            merger: Some(merger),
        }
    }

    pub fn events(&self) -> &Receiver<Event> {
        &self.events
    }

    /// Feeds source `i`.
    pub fn pusher(&self, i: usize) -> Pusher {
        self.engines[i].pusher()
    }

    pub fn break_paragraph(&self) {
        for e in &self.engines {
            e.break_paragraph();
        }
    }

    pub fn set_paragraph_mode(&self, mode: ParagraphMode) {
        for e in &self.engines {
            e.set_paragraph_mode(mode);
        }
    }

    /// Drain every engine (in parallel) and merge their transcripts in
    /// time order.
    pub fn finish(mut self) -> Transcript {
        let labelled = self.merger.is_some();
        let parts: Vec<Transcript> = std::thread::scope(|sc| {
            let jobs: Vec<_> = self
                .engines
                .drain(..)
                .map(|e| sc.spawn(move || e.finish()))
                .collect();
            jobs.into_iter()
                .map(|j| j.join().unwrap_or_default())
                .collect()
        });
        if let Some(m) = self.merger.take() {
            let _ = m.join();
        }
        if !labelled {
            return parts.into_iter().next().unwrap_or_default();
        }
        let mut paragraphs: Vec<Paragraph> = parts
            .into_iter()
            .enumerate()
            .flat_map(|(i, t)| {
                t.paragraphs.into_iter().map(move |mut p| {
                    stamp(&mut p, i);
                    p
                })
            })
            .collect();
        paragraphs.sort_by_key(|p| p.start_ms);
        Transcript { paragraphs }
    }
}

/// Label `p` (and its clips) as source `i`'s speaker.
fn stamp(p: &mut Paragraph, i: usize) {
    let l = label(i);
    for c in &mut p.clips {
        c.speaker = Some(l.clone());
    }
    p.speaker = Some(l);
}

/// Forward every engine's events until all have finished. The level
/// meter shows the loudest source.
fn merge(rxs: Vec<Receiver<Event>>, tx: Sender<Event>) {
    let mut levels = vec![(0.0f32, false, 0u64); rxs.len()];
    let mut sel = Select::new();
    for rx in &rxs {
        sel.recv(rx);
    }
    let mut open = rxs.len();
    while open > 0 {
        let op = sel.select();
        let i = op.index();
        let ev = match op.recv(&rxs[i]) {
            Ok(ev) => ev,
            Err(_) => {
                sel.remove(i);
                open -= 1;
                continue;
            }
        };
        let ev = match ev {
            Event::Paragraph {
                mut paragraph,
                change,
            } => {
                stamp(&mut paragraph, i);
                Event::Paragraph { paragraph, change }
            }
            Event::Level {
                rms,
                speaking,
                position_ms,
            } => {
                levels[i] = (rms, speaking, position_ms);
                Event::Level {
                    rms: levels.iter().map(|l| l.0).fold(0.0, f32::max),
                    speaking: levels.iter().any(|l| l.1),
                    position_ms: levels.iter().map(|l| l.2).max().unwrap_or(0),
                }
            }
            other => other,
        };
        let _ = tx.send(ev);
    }
}

/// Mixes several sources into one --record file. Each source's audio is
/// resampled to 48 kHz and queued; a stretch is written once every
/// source has supplied it, so the sources stay aligned by sample count.
pub struct Mixer {
    writer: Option<OpusWriter>,
    tracks: Vec<Track>,
}

struct Track {
    resampler: Linear,
    queue: VecDeque<f32>,
    done: bool,
}

/// How far one source may run ahead of a stalled one (in 48 kHz
/// samples) before the stalled one is taken as silence.
const SLACK: usize = 2 * RATE as usize;

impl Mixer {
    /// `rates`: each source's sample rate, in source order.
    pub fn create(path: &Path, rates: &[u32]) -> Result<Self> {
        Ok(Self {
            writer: Some(OpusWriter::create(path, RATE)?),
            tracks: rates
                .iter()
                .map(|&r| Track {
                    resampler: Linear::new(r, RATE),
                    queue: VecDeque::new(),
                    done: false,
                })
                .collect(),
        })
    }

    pub fn push(&mut self, i: usize, mono: &[f32]) {
        let mut out = Vec::with_capacity(mono.len() * 2);
        self.tracks[i].resampler.process(mono, &mut out);
        self.tracks[i].queue.extend(out);
        self.drain(false);
    }

    /// Source `i` has ended; once all have, the file is finished.
    pub fn done(&mut self, i: usize) {
        self.tracks[i].done = true;
        if self.tracks.iter().all(|t| t.done) {
            self.drain(true);
            if let Some(w) = self.writer.take() {
                if let Err(e) = w.finish() {
                    tracing::error!("finishing the recording failed: {e:#}");
                }
            }
        }
    }

    fn drain(&mut self, all: bool) {
        let longest = self.tracks.iter().map(|t| t.queue.len()).max().unwrap_or(0);
        let n = if all {
            longest
        } else {
            let ready = self
                .tracks
                .iter()
                .filter(|t| !t.done)
                .map(|t| t.queue.len())
                .min()
                .unwrap_or(longest);
            ready.max(longest.saturating_sub(SLACK))
        };
        if n == 0 {
            return;
        }
        let mut out = vec![0.0f32; n];
        for t in &mut self.tracks {
            let k = n.min(t.queue.len());
            for (o, s) in out.iter_mut().zip(t.queue.drain(..k)) {
                *o += s;
            }
        }
        for s in &mut out {
            *s = s.clamp(-1.0, 1.0);
        }
        if let Some(w) = &mut self.writer {
            if let Err(e) = w.write(&out) {
                tracing::error!("recording stopped: {e:#}");
                self.writer = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mixer(rates: &[u32]) -> (Mixer, tempdir::Dir) {
        let dir = tempdir::Dir::new();
        let m = Mixer::create(&dir.0.join("mix.opus"), rates).unwrap();
        (m, dir)
    }

    mod tempdir {
        pub struct Dir(pub std::path::PathBuf);
        impl Dir {
            pub fn new() -> Self {
                let p = std::env::temp_dir().join(format!(
                    "scribe-mix-{}-{:?}",
                    std::process::id(),
                    std::thread::current().id()
                ));
                std::fs::create_dir_all(&p).unwrap();
                Self(p)
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn mixes_only_what_every_source_has() {
        let (mut m, _d) = mixer(&[RATE, RATE]);
        m.push(0, &[0.25; 100]);
        // Source 1 hasn't caught up: nothing written yet.
        assert_eq!(m.tracks[0].queue.len(), 100);
        m.push(1, &[0.5; 60]);
        assert_eq!(m.tracks[0].queue.len(), 40);
        assert_eq!(m.tracks[1].queue.len(), 0);
    }

    #[test]
    fn a_stalled_source_is_silence_past_the_slack() {
        let (mut m, _d) = mixer(&[RATE, RATE]);
        m.push(0, &vec![0.1; SLACK + 500]);
        assert_eq!(m.tracks[0].queue.len(), SLACK);
    }

    #[test]
    fn finished_sources_stop_holding_back_the_rest() {
        let (mut m, _d) = mixer(&[RATE, RATE]);
        m.done(1);
        m.push(0, &[0.1; 100]);
        assert!(m.tracks[0].queue.is_empty());
        m.done(0);
        assert!(m.writer.is_none());
    }

    #[test]
    fn stamps_paragraph_and_clips() {
        let clip = vox_transcribe::Clip {
            id: "c".into(),
            start_ms: 0,
            duration_ms: Some(1000),
            text: "hi".into(),
            stage: vox_transcribe::Stage::Final,
            words: Vec::new(),
            speaker: None,
        };
        let mut p = Paragraph {
            id: "p".into(),
            start_ms: 0,
            end_ms: 1000,
            text: "hi".into(),
            clips: vec![clip],
            words: Vec::new(),
            speaker: None,
            closed: false,
            hardened: false,
            pass3_inflight: false,
            pass4: None,
        };
        stamp(&mut p, 1);
        assert_eq!(p.speaker.as_deref(), Some("B"));
        assert_eq!(p.clips[0].speaker.as_deref(), Some("B"));
    }
}
