//! Several audio sources at once (`-d mic1 -d mic2 -a Discord`), one
//! speaker each: every source gets its own engine, so overlapping speech
//! is transcribed channel by channel rather than diarized from a mix.
//! Their events merge into one stream with each paragraph labelled by
//! its source (A, B, … in command-line order), and --record mixes the
//! sources into one .opus. When one speaker starts talking in the middle
//! of another's paragraph, that paragraph is split at the nearest
//! sentence end, so the transcript reads as a conversation.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::thread::JoinHandle;

use anyhow::Result;
use crossbeam_channel::{unbounded, Receiver, Select, Sender};
use vox_audio::opus_file::{OpusWriter, RATE};
use vox_audio::resample::Linear;
use vox_transcribe::filters::ends_on_sentence;
use vox_transcribe::speaker::label;
use vox_transcribe::{
    Change, Clip, Engine, Event, Paragraph, ParagraphMode, Pass4, Pusher, Transcript, Word,
};

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
        let whole: Vec<Paragraph> = parts
            .into_iter()
            .enumerate()
            .flat_map(|(i, t)| {
                t.paragraphs.into_iter().map(move |mut p| {
                    stamp(&mut p, i);
                    p
                })
            })
            .collect();
        let mut paragraphs: Vec<Paragraph> = whole
            .iter()
            .flat_map(|p| split_at(p, &interruptions(p, whole.iter())))
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

/// When other speakers started talking during `p`: the starts of their
/// paragraphs that fall inside it.
fn interruptions<'a>(p: &Paragraph, all: impl Iterator<Item = &'a Paragraph>) -> Vec<u64> {
    all.filter(|q| q.speaker != p.speaker && !q.text.trim().is_empty())
        .map(|q| q.start_ms)
        .filter(|&t| t > p.start_ms && t < p.end_ms)
        .collect()
}

/// Split `p` at the sentence end nearest each time in `cuts`. The first
/// piece keeps `p`'s id; the others get `<id>~1`, `<id>~2`, …. Only
/// finished (timed) words are split; a trailing partial stays with the
/// last piece. A piece after a cut starts no earlier than just after the
/// interruption, so in time order the interrupter's paragraph falls
/// between the two (the words keep their real times).
pub fn split_at(p: &Paragraph, cuts: &[u64]) -> Vec<Paragraph> {
    let tokens: Vec<&str> = p.text.split_whitespace().collect();
    let words = &p.words;
    if cuts.is_empty() || words.len() < 2 || words.len() > tokens.len() {
        return vec![p.clone()];
    }
    // Split points: before word k when word k-1 ends a sentence, at the
    // middle of the pause between them.
    let ends: Vec<(usize, u64)> = (1..words.len())
        .filter(|&k| ends_on_sentence(tokens[k - 1]))
        .map(|k| {
            let (a, b) = (words[k - 1].end_ms, words[k].start_ms);
            (k, (a + b.max(a)) / 2)
        })
        .collect();
    // Split point → (its time, the latest interruption it serves).
    let mut at: std::collections::BTreeMap<usize, (u64, u64)> = Default::default();
    for &t in cuts {
        if let Some(&(k, e)) = ends.iter().min_by_key(|(_, e)| e.abs_diff(t)) {
            let slot = at.entry(k).or_insert((e, t));
            slot.1 = slot.1.max(t);
        }
    }
    if at.is_empty() {
        return vec![p.clone()];
    }
    // The clips split by word when their words make up the text (no
    // pass-4 rewrite), else by time.
    let by_word = p
        .clips
        .iter()
        .map(|c| c.text.split_whitespace().count())
        .sum::<usize>()
        == tokens.len();
    let mut bounds: Vec<(usize, u64)> = vec![(0, 0)];
    bounds.extend(at.iter().map(|(&k, &(e, _))| (k, e)));
    bounds.push((tokens.len(), u64::MAX));
    let mut after: Vec<u64> = vec![0];
    after.extend(at.values().map(|&(_, t)| t + 1));
    let last = bounds.len() - 2;
    bounds
        .windows(2)
        .enumerate()
        .map(|(i, w)| {
            let ((a, from_ms), (b, to_ms)) = (w[0], w[1]);
            let text = tokens[a..b].join(" ");
            let ws: Vec<Word> = words[a.min(words.len())..b.min(words.len())].to_vec();
            let clips = if by_word {
                clips_between(&p.clips, a, b)
            } else {
                p.clips
                    .iter()
                    .filter(|c| c.start_ms >= from_ms && c.start_ms < to_ms)
                    .cloned()
                    .collect()
            };
            Paragraph {
                id: if i == 0 {
                    p.id.clone()
                } else {
                    format!("{}~{i}", p.id)
                },
                start_ms: if i == 0 {
                    p.start_ms
                } else {
                    ws.first().map_or(from_ms, |w| w.start_ms).max(after[i])
                },
                end_ms: if i == last {
                    p.end_ms
                } else {
                    ws.last().map_or(to_ms, |w| w.end_ms)
                },
                pass4: match &p.pass4 {
                    // The before/after diff can't be split; show the
                    // pieces as they are.
                    Some(Pass4::Done { .. }) => Some(Pass4::Done {
                        original: text.clone(),
                        edits: 0,
                    }),
                    other => other.clone(),
                },
                text,
                words: ws,
                clips,
                speaker: p.speaker.clone(),
                closed: p.closed || i < last,
                hardened: p.hardened,
                pass3_inflight: p.pass3_inflight,
            }
        })
        .collect()
}

/// The parts of `clips` covering words `a..b` of their joined text; a
/// clip that straddles a bound is cut there.
fn clips_between(clips: &[Clip], a: usize, b: usize) -> Vec<Clip> {
    let mut out = Vec::new();
    let mut at = 0;
    for c in clips {
        let toks: Vec<&str> = c.text.split_whitespace().collect();
        let (lo, hi) = (a.max(at), b.min(at + toks.len()));
        if lo < hi {
            let (i, j) = (lo - at, hi - at);
            if i == 0 && j == toks.len() {
                out.push(c.clone());
            } else {
                let timed = if c.is_partial() {
                    Vec::new()
                } else {
                    c.timed_words()
                };
                let ws: Vec<Word> = timed.get(i..j).map(<[Word]>::to_vec).unwrap_or_default();
                let start_ms = ws.first().map_or(c.start_ms, |w| w.start_ms);
                out.push(Clip {
                    id: if i == 0 {
                        c.id.clone()
                    } else {
                        format!("{}~{i}", c.id)
                    },
                    start_ms,
                    duration_ms: match ws.last() {
                        Some(w) => Some(w.end_ms.saturating_sub(start_ms)),
                        None => c.duration_ms,
                    },
                    text: toks[i..j].join(" "),
                    stage: c.stage,
                    words: ws,
                    speaker: c.speaker.clone(),
                });
            }
        }
        at += toks.len();
    }
    out
}

/// The merger's view of every source's paragraphs, re-split as other
/// speakers interrupt them.
#[derive(Default)]
struct Splitter {
    /// Latest unsplit paragraph by id.
    whole: HashMap<String, Paragraph>,
    /// Piece ids last sent for each paragraph.
    sent: HashMap<String, Vec<String>>,
}

impl Splitter {
    /// `p` changed: send its pieces, then re-split the open paragraphs
    /// of other speakers it now interrupts.
    fn update(&mut self, p: Paragraph, change: Change, tx: &Sender<Event>) {
        let (start, speaker) = (p.start_ms, p.speaker.clone());
        self.whole.insert(p.id.clone(), p.clone());
        self.send(&p, change, tx);
        let others: Vec<Paragraph> = self
            .whole
            .values()
            .filter(|q| {
                q.speaker != speaker && !q.hardened && q.start_ms < start && start < q.end_ms
            })
            .cloned()
            .collect();
        for q in others {
            self.resend_if_split_changed(&q, tx);
        }
    }

    fn remove(&mut self, id: &str, tx: &Sender<Event>) {
        let Some(p) = self.whole.remove(id) else {
            let _ = tx.send(Event::ParagraphRemoved { id: id.into() });
            return;
        };
        for piece in self.sent.remove(id).unwrap_or_default() {
            let _ = tx.send(Event::ParagraphRemoved { id: piece });
        }
        let others: Vec<Paragraph> = self
            .whole
            .values()
            .filter(|q| q.speaker != p.speaker && !q.hardened && q.start_ms < p.start_ms)
            .cloned()
            .collect();
        for q in others {
            self.resend_if_split_changed(&q, tx);
        }
    }

    fn pieces(&self, p: &Paragraph) -> Vec<Paragraph> {
        split_at(p, &interruptions(p, self.whole.values()))
    }

    fn resend_if_split_changed(&mut self, q: &Paragraph, tx: &Sender<Event>) {
        let ids: Vec<String> = self.pieces(q).into_iter().map(|p| p.id).collect();
        if self.sent.get(&q.id) != Some(&ids) {
            self.send(q, Change::Revised, tx);
        }
    }

    fn send(&mut self, p: &Paragraph, change: Change, tx: &Sender<Event>) {
        let pieces = self.pieces(p);
        let ids: Vec<String> = pieces.iter().map(|q| q.id.clone()).collect();
        for old in self.sent.get(&p.id).into_iter().flatten() {
            if !ids.contains(old) {
                let _ = tx.send(Event::ParagraphRemoved { id: old.clone() });
            }
        }
        for paragraph in pieces {
            let _ = tx.send(Event::Paragraph { paragraph, change });
        }
        self.sent.insert(p.id.clone(), ids);
    }
}

/// Forward every engine's events until all have finished. The level
/// meter shows the loudest source.
fn merge(rxs: Vec<Receiver<Event>>, tx: Sender<Event>) {
    let mut levels = vec![(0.0f32, false, 0u64); rxs.len()];
    let mut splitter = Splitter::default();
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
                splitter.update(paragraph, change, &tx);
                continue;
            }
            Event::ParagraphRemoved { id } => {
                splitter.remove(&id, &tx);
                continue;
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

    /// One clip per sentence, a word every 500 ms.
    fn talk(sentences: &[&str]) -> Paragraph {
        let mut clips = Vec::new();
        let mut words = Vec::new();
        let mut t = 0;
        for (n, s) in sentences.iter().enumerate() {
            let start = t;
            let ws: Vec<Word> = s
                .split_whitespace()
                .map(|w| {
                    t += 500;
                    Word {
                        text: w.into(),
                        start_ms: t - 500,
                        end_ms: t - 100,
                    }
                })
                .collect();
            words.extend(ws.clone());
            clips.push(vox_transcribe::Clip {
                id: format!("c{n}"),
                start_ms: start,
                duration_ms: Some(t - start),
                text: s.to_string(),
                stage: vox_transcribe::Stage::Final,
                words: ws,
                speaker: Some("A".into()),
            });
        }
        Paragraph {
            id: "p".into(),
            start_ms: 0,
            end_ms: t,
            text: sentences.join(" "),
            clips,
            words,
            speaker: Some("A".into()),
            closed: false,
            hardened: false,
            pass3_inflight: false,
            pass4: None,
        }
    }

    fn texts(ps: &[Paragraph]) -> Vec<&str> {
        ps.iter().map(|p| p.text.as_str()).collect()
    }

    #[test]
    fn splits_at_the_nearest_sentence_end() {
        let p = talk(&["Hello there.", "How are you today?", "I am fine."]);
        // "there." ends at 900 ms, "today?" at 2900: 2.2 s is nearer the
        // first.
        let pieces = split_at(&p, &[1800]);
        assert_eq!(
            texts(&pieces),
            ["Hello there.", "How are you today? I am fine."]
        );
        assert_eq!(pieces[0].id, "p");
        assert_eq!(pieces[1].id, "p~1");
        assert!(pieces[0].closed);
        // Resumes at 1 s, but after the interruption at 1.8 s.
        assert_eq!(pieces[1].start_ms, 1801);
        assert_eq!(pieces[1].clips.len(), 2);
        assert_eq!(texts(&split_at(&p, &[2700])).len(), 2);
        assert_eq!(texts(&split_at(&p, &[2700]))[1], "I am fine.");
    }

    #[test]
    fn a_sentence_split_across_clips_cuts_the_clip() {
        let p = talk(&["Yes. And then", "we left."]);
        let pieces = split_at(&p, &[600]);
        assert_eq!(texts(&pieces), ["Yes.", "And then we left."]);
        let first: Vec<&str> = pieces[1].clips.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(first, ["And then", "we left."]);
        assert_eq!(pieces[1].clips[0].id, "c0~1");
        assert_eq!(pieces[1].clips[0].start_ms, 500);
    }

    #[test]
    fn no_sentence_end_no_split() {
        let p = talk(&["and so on and so forth"]);
        assert_eq!(split_at(&p, &[1000]).len(), 1);
        assert_eq!(split_at(&talk(&["One.", "Two."]), &[]).len(), 1);
    }

    #[test]
    fn interruptions_are_other_speakers_starting_inside() {
        let p = talk(&["Hello there.", "How are you today?"]);
        let mut q = talk(&["Hi."]);
        q.speaker = Some("B".into());
        q.start_ms = 1200;
        let mut own = q.clone();
        own.speaker = Some("A".into());
        let mut late = q.clone();
        late.start_ms = 9_000;
        assert_eq!(interruptions(&p, [&q, &own, &late].into_iter()), [1200]);
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
