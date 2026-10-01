//! The layered pipeline, wired together for one audio channel.
//!
//! ```text
//! push(mono) ─▶ control thread ─┬─ VAD ─▶ pass 1: streaming partials (inline)
//!                               │         utterance end ─▶ pass-2 job
//!                               ├─ audio ring ──────────▶ pass-3 job
//!                               └─ applies results, emits Events
//! offline worker thread: runs pass-2 jobs before pass-3 jobs
//! ```
//!
//! All transcript state lives on the control thread, so there are no
//! locks and no stale-partial races: a partial can only be emitted for
//! the utterance currently being spoken.

use std::collections::HashSet;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, never, select, unbounded, Receiver, Sender};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::boundary::{self, BoundaryConfig, WindowClip};
use crate::correct::{apply_edits, Corrector, Edit};
use crate::filters::{count_words, ends_on_sentence, is_junk};
use crate::model::{Clip, Paragraph, Pass4, Stage, Transcript};
use crate::recognizer::{OfflineRecognizer, StreamingRecognizer};
use crate::ring::AudioRing;
use crate::vad::{EndReason, Vad, VadConfig, VadEvent};

/// How paragraphs are split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParagraphMode {
    /// A silence gap or a word cap opens a new paragraph.
    #[default]
    Auto,
    /// Only [`Engine::break_paragraph`] opens a new paragraph.
    Manual,
}

#[derive(Debug, Clone)]
pub struct ParagraphConfig {
    pub mode: ParagraphMode,
    /// Silence that opens a new paragraph, and after which an idle
    /// paragraph is hardened.
    pub gap_ms: u64,
    /// Past this many words, close the paragraph at the next natural
    /// boundary (a silence-closed utterance, or a pass-3 sentence end).
    pub soft_max_words: usize,
    /// Past this many words, close the paragraph unconditionally.
    pub hard_max_words: usize,
}

impl Default for ParagraphConfig {
    fn default() -> Self {
        Self {
            mode: ParagraphMode::Auto,
            gap_ms: 2_000,
            soft_max_words: 100,
            hard_max_words: 200,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Rate of the mono samples passed to [`Engine::push`].
    pub sample_rate: u32,
    pub vad: VadConfig,
    pub paragraph: ParagraphConfig,
    pub boundary: BoundaryConfig,
    /// Audio history kept for pass 3.
    pub ring_ms: u64,
    /// Minimum wall time between pass-1 partial updates.
    pub partial_interval: Duration,
    /// Audio time between [`Event::Level`] updates.
    pub level_interval_ms: u64,
}

impl EngineConfig {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            vad: VadConfig::default(),
            paragraph: ParagraphConfig::default(),
            boundary: BoundaryConfig::default(),
            ring_ms: 90_000,
            partial_interval: Duration::from_millis(150),
            level_interval_ms: 50,
        }
    }
}

/// What just happened to a paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Opened,
    /// Pass 1 updated a streaming partial.
    Partial,
    /// Pass 2 replaced a partial (or added a clip).
    Final,
    /// Pass 3 rewrote clips at a boundary.
    Revised,
    ClipRemoved,
    /// Word cap reached; the next clip opens a new paragraph.
    Closed,
    /// Pass 4 finished (see [`Paragraph::pass4`] for the outcome).
    Corrected,
    /// No further passes will touch it.
    Hardened,
}

#[derive(Debug, Clone)]
pub enum Event {
    /// Full current state of one paragraph; upsert by id.
    Paragraph {
        paragraph: Paragraph,
        change: Change,
    },
    ParagraphRemoved {
        id: String,
    },
    /// Input level for meters. `rms` is the loudest 20 ms window since
    /// the previous level event.
    Level {
        rms: f32,
        speaking: bool,
        position_ms: u64,
    },
}

enum Cmd {
    Audio(Vec<f32>),
    Break,
    Mode(ParagraphMode),
    Finish(Sender<Transcript>),
}

enum Job {
    Final {
        clip_id: String,
        start_ms: u64,
        duration_ms: u64,
        reason: EndReason,
        samples: Vec<f32>,
    },
    Boundary {
        paragraph_id: String,
        window: Vec<WindowClip>,
        samples: Vec<f32>,
    },
}

struct LlmJob {
    paragraph_id: String,
    text: String,
    context: Vec<String>,
}

struct LlmResult {
    job: LlmJob,
    edits: anyhow::Result<Vec<Edit>>,
}

struct JobResult {
    job: Job,
    text: anyhow::Result<String>,
}

/// Handle to a running pipeline. Dropping it stops the threads without
/// waiting for queued passes; call [`Engine::finish`] to drain them.
pub struct Engine {
    cmd_tx: Sender<Cmd>,
    events: Receiver<Event>,
    control: Option<JoinHandle<()>>,
}

impl Engine {
    /// Start the control and offline-worker threads. `streaming` may be
    /// `None`, in which case clips first appear at pass 2.
    pub fn spawn(
        cfg: EngineConfig,
        streaming: Option<Box<dyn StreamingRecognizer>>,
        offline: Arc<dyn OfflineRecognizer>,
    ) -> Self {
        Self::spawn_with(cfg, streaming, offline, None)
    }

    /// Like [`Engine::spawn`], with an optional pass-4 corrector. When
    /// given, every paragraph goes through it before it hardens.
    pub fn spawn_with(
        cfg: EngineConfig,
        streaming: Option<Box<dyn StreamingRecognizer>>,
        offline: Arc<dyn OfflineRecognizer>,
        corrector: Option<Arc<dyn Corrector>>,
    ) -> Self {
        let (cmd_tx, cmd_rx) = bounded::<Cmd>(64);
        let (ev_tx, ev_rx) = unbounded::<Event>();
        let (final_tx, final_rx) = unbounded::<Job>();
        let (bound_tx, bound_rx) = unbounded::<Job>();
        let (res_tx, res_rx) = unbounded::<JobResult>();
        let rate = cfg.sample_rate;

        std::thread::Builder::new()
            .name("vox-offline".into())
            .spawn(move || offline_worker(offline, rate, final_rx, bound_rx, res_tx))
            .expect("spawn offline worker");

        // Pass 4 gets its own thread: LLM calls are slow and network-bound
        // and must not hold up passes 2 and 3.
        let (llm_tx, llm_rx) = match corrector {
            Some(c) => {
                let (job_tx, job_rx) = unbounded::<LlmJob>();
                let (out_tx, out_rx) = unbounded::<LlmResult>();
                std::thread::Builder::new()
                    .name("vox-llm".into())
                    .spawn(move || {
                        for job in job_rx {
                            let edits = c.correct(&job.text, &job.context);
                            if out_tx.send(LlmResult { job, edits }).is_err() {
                                return;
                            }
                        }
                    })
                    .expect("spawn llm worker");
                (Some(job_tx), out_rx)
            }
            None => (None, never()),
        };

        let control = std::thread::Builder::new()
            .name("vox-control".into())
            .spawn(move || {
                let mut core = Core::new(cfg, streaming, ev_tx, final_tx, bound_tx);
                core.llm_tx = llm_tx;
                core.run(cmd_rx, res_rx, llm_rx);
            })
            .expect("spawn control thread");

        Self {
            cmd_tx,
            events: ev_rx,
            control: Some(control),
        }
    }

    /// Feed mono samples. Blocks briefly if the control thread is behind.
    pub fn push(&self, mono: &[f32]) {
        let _ = self.cmd_tx.send(Cmd::Audio(mono.to_vec()));
    }

    /// Close the current utterance and start a new paragraph with the
    /// next clip (e.g. the user paused capture or pressed a key).
    pub fn break_paragraph(&self) {
        let _ = self.cmd_tx.send(Cmd::Break);
    }

    /// Switch between automatic and Enter-only paragraph breaks. Takes
    /// effect from the next clip.
    pub fn set_paragraph_mode(&self, mode: ParagraphMode) {
        let _ = self.cmd_tx.send(Cmd::Mode(mode));
    }

    /// A cloneable handle for feeding audio from another thread while
    /// this `Engine` stays put for [`Engine::finish`].
    pub fn pusher(&self) -> Pusher {
        Pusher {
            cmd_tx: self.cmd_tx.clone(),
        }
    }

    pub fn events(&self) -> &Receiver<Event> {
        &self.events
    }

    /// Flush the open utterance, wait for every queued pass, harden all
    /// paragraphs and return the final transcript. Events stay readable
    /// on the receiver returned by [`Engine::events`] (clone it first).
    pub fn finish(mut self) -> Transcript {
        let (tx, rx) = bounded(1);
        let _ = self.cmd_tx.send(Cmd::Finish(tx));
        let t = rx.recv().unwrap_or_default();
        if let Some(h) = self.control.take() {
            let _ = h.join();
        }
        t
    }
}

/// See [`Engine::pusher`]. Pushes after `finish` are silently dropped.
#[derive(Clone)]
pub struct Pusher {
    cmd_tx: Sender<Cmd>,
}

impl Pusher {
    pub fn push(&self, mono: &[f32]) {
        let _ = self.cmd_tx.send(Cmd::Audio(mono.to_vec()));
    }

    pub fn break_paragraph(&self) {
        let _ = self.cmd_tx.send(Cmd::Break);
    }
}

fn offline_worker(
    rec: Arc<dyn OfflineRecognizer>,
    rate: u32,
    final_rx: Receiver<Job>,
    bound_rx: Receiver<Job>,
    res_tx: Sender<JobResult>,
) {
    loop {
        // Pass 2 always jumps the queue: it's what replaces the ALL CAPS
        // partial on screen, while pass 3 is polish.
        let job = match final_rx.try_recv() {
            Ok(j) => j,
            Err(_) => select! {
                recv(final_rx) -> j => match j { Ok(j) => j, Err(_) => return },
                recv(bound_rx) -> j => match j { Ok(j) => j, Err(_) => return },
            },
        };
        let samples = match &job {
            Job::Final { samples, .. } | Job::Boundary { samples, .. } => samples,
        };
        let text = rec.transcribe(samples, rate);
        if res_tx.send(JobResult { job, text }).is_err() {
            return;
        }
    }
}

struct Current {
    clip_id: String,
    start_ms: u64,
    last_partial: String,
    last_emit: Instant,
}

struct Core {
    cfg: EngineConfig,
    vad: Vad,
    ring: AudioRing,
    streaming: Option<Box<dyn StreamingRecognizer>>,
    transcript: Transcript,
    current: Option<Current>,
    events: Sender<Event>,
    final_tx: Sender<Job>,
    bound_tx: Sender<Job>,
    outstanding_final: usize,
    outstanding_boundary: usize,
    llm_tx: Option<Sender<LlmJob>>,
    outstanding_llm: usize,
    pass3_pending: HashSet<String>,
    level_max: f32,
    level_next: u64,
}

impl Core {
    fn new(
        cfg: EngineConfig,
        streaming: Option<Box<dyn StreamingRecognizer>>,
        events: Sender<Event>,
        final_tx: Sender<Job>,
        bound_tx: Sender<Job>,
    ) -> Self {
        let rate = cfg.sample_rate;
        Self {
            vad: Vad::new(cfg.vad.clone(), rate),
            ring: AudioRing::new((rate as u64 * cfg.ring_ms / 1000) as usize),
            streaming,
            transcript: Transcript::default(),
            current: None,
            events,
            final_tx,
            bound_tx,
            outstanding_final: 0,
            outstanding_boundary: 0,
            llm_tx: None,
            outstanding_llm: 0,
            pass3_pending: HashSet::new(),
            level_max: 0.0,
            level_next: 0,
            cfg,
        }
    }

    fn run(
        &mut self,
        cmd_rx: Receiver<Cmd>,
        res_rx: Receiver<JobResult>,
        llm_rx: Receiver<LlmResult>,
    ) {
        let mut finishing: Option<Option<Sender<Transcript>>> = None;
        loop {
            if let Some(reply) = &finishing {
                let idle =
                    self.outstanding_final + self.outstanding_boundary + self.outstanding_llm == 0;
                // Once passes 2 and 3 are drained, send every remaining
                // paragraph through pass 4 before hardening it.
                if idle && self.request_pass4_all() == 0 {
                    self.harden_all();
                    if let Some(tx) = reply {
                        let _ = tx.send(self.transcript.clone());
                    }
                    return;
                }
                select! {
                    recv(res_rx) -> r => match r { Ok(r) => self.on_result(r), Err(_) => return },
                    recv(llm_rx) -> r => match r { Ok(r) => self.on_llm(r), Err(_) => return },
                }
                continue;
            }
            select! {
                recv(cmd_rx) -> m => match m {
                    Ok(Cmd::Audio(a)) => self.on_audio(&a),
                    Ok(Cmd::Break) => self.on_break(),
                    Ok(Cmd::Mode(m)) => self.cfg.paragraph.mode = m,
                    Ok(Cmd::Finish(tx)) => {
                        self.flush_vad();
                        finishing = Some(Some(tx));
                    }
                    Err(_) => {
                        self.flush_vad();
                        finishing = Some(None);
                    }
                },
                recv(res_rx) -> r => if let Ok(r) = r { self.on_result(r) },
                recv(llm_rx) -> r => if let Ok(r) = r { self.on_llm(r) },
            }
        }
    }

    fn ms(&self, samples: u64) -> u64 {
        samples * 1000 / self.cfg.sample_rate as u64
    }

    fn sample(&self, ms: u64) -> u64 {
        ms * self.cfg.sample_rate as u64 / 1000
    }

    fn now_ms(&self) -> u64 {
        self.ms(self.vad.position())
    }

    fn emit(&self, idx: usize, change: Change) {
        if let Some(p) = self.transcript.paragraphs.get(idx) {
            let _ = self.events.send(Event::Paragraph {
                paragraph: p.clone(),
                change,
            });
        }
    }

    fn find_paragraph(&self, id: &str) -> Option<usize> {
        self.transcript.paragraphs.iter().rposition(|p| p.id == id)
    }

    fn find_clip(&self, clip_id: &str) -> Option<(usize, usize)> {
        self.transcript
            .paragraphs
            .iter()
            .enumerate()
            .rev()
            .find_map(|(pi, p)| {
                p.clips
                    .iter()
                    .position(|c| c.id == clip_id)
                    .map(|ci| (pi, ci))
            })
    }

    // ----- audio path ----------------------------------------------------

    fn on_audio(&mut self, mono: &[f32]) {
        self.ring.push(mono);
        let mut evs = Vec::new();
        self.vad.push(mono, &mut evs);
        for ev in evs {
            self.on_vad(ev);
        }
        self.level_max = self.level_max.max(self.vad.last_rms());
        let pos = self.vad.position();
        if pos >= self.level_next {
            let _ = self.events.send(Event::Level {
                rms: self.level_max,
                speaking: self.vad.is_speaking(),
                position_ms: self.ms(pos),
            });
            self.level_max = 0.0;
            self.level_next = pos + self.sample(self.cfg.level_interval_ms).max(1);
        }
        self.harden_idle();
    }

    fn flush_vad(&mut self) {
        let mut evs = Vec::new();
        self.vad.flush(&mut evs);
        for ev in evs {
            self.on_vad(ev);
        }
    }

    fn on_break(&mut self) {
        self.flush_vad();
        if let Some(idx) = self.transcript.paragraphs.len().checked_sub(1) {
            let p = &mut self.transcript.paragraphs[idx];
            if !p.closed && !p.hardened {
                p.closed = true;
                self.emit(idx, Change::Closed);
            }
        }
    }

    fn on_vad(&mut self, ev: VadEvent) {
        let rate = self.cfg.sample_rate;
        match ev {
            VadEvent::Start {
                start_sample,
                audio,
            } => {
                self.current = Some(Current {
                    clip_id: Uuid::new_v4().to_string(),
                    start_ms: self.ms(start_sample),
                    last_partial: String::new(),
                    last_emit: Instant::now(),
                });
                if let Some(s) = self.streaming.as_mut() {
                    s.reset();
                    s.feed(&audio, rate);
                }
                self.maybe_partial();
            }
            VadEvent::Audio(window) => {
                if self.current.is_some() {
                    if let Some(s) = self.streaming.as_mut() {
                        s.feed(&window, rate);
                    }
                    self.maybe_partial();
                }
            }
            VadEvent::End {
                start_sample,
                samples,
                reason,
            } => {
                if let Some(s) = self.streaming.as_mut() {
                    s.reset();
                }
                let clip_id = self
                    .current
                    .take()
                    .map(|c| c.clip_id)
                    .unwrap_or_else(|| Uuid::new_v4().to_string());
                let duration_ms = self.ms(samples.len() as u64);
                // The length is known now, before pass 2 returns. Record
                // it on the partial so the next clip's paragraph-gap test
                // is exact even when pass 2 lags (e.g. file replay).
                if let Some((pi, ci)) = self.find_clip(&clip_id) {
                    let p = &mut self.transcript.paragraphs[pi];
                    p.clips[ci].duration_ms = Some(duration_ms);
                    p.rebuild();
                }
                let job = Job::Final {
                    clip_id,
                    start_ms: self.ms(start_sample),
                    duration_ms,
                    reason,
                    samples,
                };
                self.outstanding_final += 1;
                let _ = self.final_tx.send(job);
            }
            VadEvent::Discard => {
                if let Some(s) = self.streaming.as_mut() {
                    s.reset();
                }
                if let Some(c) = self.current.take() {
                    self.remove_clip(&c.clip_id);
                }
            }
        }
    }

    fn maybe_partial(&mut self) {
        let Some(s) = self.streaming.as_ref() else {
            return;
        };
        let text = s.partial();
        let interval = self.cfg.partial_interval;
        let Some(cur) = self.current.as_mut() else {
            return;
        };
        if text.is_empty() || text == cur.last_partial {
            return;
        }
        if !cur.last_partial.is_empty() && cur.last_emit.elapsed() < interval {
            return;
        }
        cur.last_partial = text.clone();
        cur.last_emit = Instant::now();
        let (id, start) = (cur.clip_id.clone(), cur.start_ms);
        if let Some((idx, change)) = self.upsert_clip(&id, text, start, None, Stage::Partial) {
            self.emit(idx, change);
        }
    }

    // ----- paragraph bookkeeping -----------------------------------------

    /// Insert or update a clip. Returns the paragraph index and whether
    /// it was opened or just changed; `None` for a stale partial.
    fn upsert_clip(
        &mut self,
        clip_id: &str,
        text: String,
        start_ms: u64,
        duration_ms: Option<u64>,
        stage: Stage,
    ) -> Option<(usize, Change)> {
        let change = if stage == Stage::Partial {
            Change::Partial
        } else {
            Change::Final
        };
        if let Some((pi, ci)) = self.find_clip(clip_id) {
            let p = &mut self.transcript.paragraphs[pi];
            let clip = &mut p.clips[ci];
            if stage == Stage::Partial && !clip.is_partial() {
                return None;
            }
            clip.text = text;
            clip.start_ms = start_ms;
            clip.duration_ms = duration_ms;
            clip.stage = stage;
            p.rebuild();
            return Some((pi, change));
        }
        let clip = Clip {
            id: clip_id.to_string(),
            start_ms,
            duration_ms,
            text,
            stage,
        };
        let max_utt = self.cfg.vad.max_utterance_ms as u64;
        let gap = self.cfg.paragraph.gap_ms;
        let need_new = match self.transcript.paragraphs.last() {
            None => true,
            Some(p) if p.closed || p.hardened => true,
            Some(_) if self.cfg.paragraph.mode == ParagraphMode::Manual => false,
            Some(p) => match p.clips.last() {
                None => true,
                Some(last) => {
                    // A still-partial previous clip has no duration yet;
                    // assume it ran to the VAD max so continuous speech
                    // doesn't spuriously open a new paragraph.
                    let last_end = match last.duration_ms {
                        Some(d) => last.start_ms + d,
                        None if last.is_partial() => last.start_ms + max_utt,
                        None => last.start_ms,
                    };
                    start_ms.saturating_sub(last_end) > gap
                }
            },
        };
        if need_new {
            self.transcript
                .paragraphs
                .push(Paragraph::new(Uuid::new_v4().to_string(), clip));
            Some((self.transcript.paragraphs.len() - 1, Change::Opened))
        } else {
            let idx = self.transcript.paragraphs.len() - 1;
            let p = &mut self.transcript.paragraphs[idx];
            p.clips.push(clip);
            p.rebuild();
            Some((idx, change))
        }
    }

    fn remove_clip(&mut self, clip_id: &str) {
        let Some((pi, ci)) = self.find_clip(clip_id) else {
            return;
        };
        let p = &mut self.transcript.paragraphs[pi];
        p.clips.remove(ci);
        if p.clips.is_empty() {
            let id = p.id.clone();
            self.transcript.paragraphs.remove(pi);
            let _ = self.events.send(Event::ParagraphRemoved { id });
        } else {
            p.rebuild();
            self.emit(pi, Change::ClipRemoved);
        }
    }

    /// Harden paragraphs that have been quiet for the paragraph gap and
    /// have nothing in flight. The last paragraph also waits for the VAD
    /// to be idle and for any pass-2 decode that might extend it.
    fn harden_idle(&mut self) {
        let now = self.now_ms();
        let gap = self.cfg.paragraph.gap_ms;
        let speaking = self.vad.is_speaking() || self.current.is_some();
        let last = self.transcript.paragraphs.len().saturating_sub(1);
        for idx in 0..self.transcript.paragraphs.len() {
            let p = &self.transcript.paragraphs[idx];
            if p.hardened
                || p.has_partial()
                || p.pass3_inflight
                || self.pass3_pending.contains(&p.id)
            {
                continue;
            }
            if now.saturating_sub(p.end_ms) < gap {
                continue;
            }
            if idx == last && (speaking || self.outstanding_final > 0) {
                continue;
            }
            // In manual mode the open paragraph stays open through any
            // silence until the user breaks it.
            if idx == last && self.cfg.paragraph.mode == ParagraphMode::Manual && !p.closed {
                continue;
            }
            // Pass 4 has the last word before hardening.
            if self.llm_tx.is_some() && !p.text.trim().is_empty() {
                match p.pass4 {
                    None => {
                        self.request_pass4(idx);
                        continue;
                    }
                    Some(Pass4::Running) => continue,
                    Some(_) => {}
                }
            }
            self.transcript.paragraphs[idx].hardened = true;
            self.emit(idx, Change::Hardened);
        }
    }

    /// Queue pass 4 for `paragraphs[idx]`. Closes the paragraph so new
    /// speech can't land in it while the LLM is working.
    fn request_pass4(&mut self, idx: usize) {
        let Some(tx) = &self.llm_tx else { return };
        let context = self.transcript.paragraphs[idx.saturating_sub(2)..idx]
            .iter()
            .map(|p| p.text.clone())
            .collect();
        let p = &mut self.transcript.paragraphs[idx];
        p.closed = true;
        p.pass4 = Some(Pass4::Running);
        let job = LlmJob {
            paragraph_id: p.id.clone(),
            text: p.text.clone(),
            context,
        };
        if tx.send(job).is_ok() {
            self.outstanding_llm += 1;
        }
        self.emit(idx, Change::Closed);
    }

    /// Queue pass 4 for every paragraph that hasn't had it. Returns how
    /// many were queued.
    fn request_pass4_all(&mut self) -> usize {
        if self.llm_tx.is_none() {
            return 0;
        }
        let todo: Vec<usize> = (0..self.transcript.paragraphs.len())
            .filter(|&i| {
                let p = &self.transcript.paragraphs[i];
                !p.hardened && p.pass4.is_none() && !p.text.trim().is_empty()
            })
            .collect();
        for &i in &todo {
            self.request_pass4(i);
        }
        todo.len()
    }

    fn on_llm(&mut self, r: LlmResult) {
        self.outstanding_llm -= 1;
        let Some(idx) = self.find_paragraph(&r.job.paragraph_id) else {
            return;
        };
        let p = &mut self.transcript.paragraphs[idx];
        let outcome = r
            .edits
            .map_err(|e| format!("{e:#}"))
            .and_then(|edits| apply_edits(&r.job.text, &edits));
        p.pass4 = Some(match outcome {
            Ok((text, edits)) => {
                let original = std::mem::replace(&mut p.text, text);
                Pass4::Done { original, edits }
            }
            Err(reason) => {
                warn!(paragraph = %p.id, %reason, "pass 4 failed; keeping pass-3 text");
                Pass4::Failed { reason }
            }
        });
        self.emit(idx, Change::Corrected);
        self.harden_idle();
    }

    fn harden_all(&mut self) {
        for idx in 0..self.transcript.paragraphs.len() {
            let p = &mut self.transcript.paragraphs[idx];
            if !p.hardened {
                p.hardened = true;
                p.pass3_inflight = false;
                self.emit(idx, Change::Hardened);
            }
        }
    }

    // ----- pass 2 / pass 3 results ---------------------------------------

    fn on_result(&mut self, r: JobResult) {
        match r.job {
            Job::Final {
                clip_id,
                start_ms,
                duration_ms,
                reason,
                ..
            } => {
                self.outstanding_final -= 1;
                self.on_final(clip_id, start_ms, duration_ms, reason, r.text);
            }
            Job::Boundary {
                paragraph_id,
                window,
                ..
            } => {
                self.outstanding_boundary -= 1;
                self.on_boundary(paragraph_id, window, r.text);
            }
        }
        self.harden_idle();
    }

    fn on_final(
        &mut self,
        clip_id: String,
        start_ms: u64,
        duration_ms: u64,
        reason: EndReason,
        text: anyhow::Result<String>,
    ) {
        let text = match text {
            Ok(t) => t.trim().to_string(),
            Err(err) => {
                warn!(err = %format!("{err:#}"), "pass 2 decode failed");
                String::new()
            }
        };
        if is_junk(&text) {
            self.remove_clip(&clip_id);
            return;
        }
        let Some((idx, change)) =
            self.upsert_clip(&clip_id, text, start_ms, Some(duration_ms), Stage::Final)
        else {
            return;
        };
        // Word caps. The soft cap only fires on a silence-closed
        // utterance (a real pause); the hard cap fires regardless, for
        // non-stop speech where every utterance is a max-length rollover.
        let pc = &self.cfg.paragraph;
        let p = &mut self.transcript.paragraphs[idx];
        let words = count_words(&p.text);
        let silence_closed = reason != EndReason::MaxLength;
        if pc.mode == ParagraphMode::Auto
            && !p.closed
            && (words >= pc.hard_max_words || (silence_closed && words >= pc.soft_max_words))
        {
            p.closed = true;
        }
        let id = p.id.clone();
        self.emit(idx, change);
        self.schedule_boundary(&id);
    }

    fn schedule_boundary(&mut self, paragraph_id: &str) {
        let Some(idx) = self.find_paragraph(paragraph_id) else {
            return;
        };
        if self.transcript.paragraphs[idx].pass3_inflight {
            self.pass3_pending.insert(paragraph_id.to_string());
            return;
        }
        let Some(window) = boundary::select_window(
            &self.transcript.paragraphs,
            idx,
            self.now_ms(),
            self.cfg.paragraph.gap_ms,
            &self.cfg.boundary,
        ) else {
            return;
        };
        let start = window.first().map(|w| w.start_ms).unwrap_or(0);
        let end = window
            .last()
            .map(|w| w.start_ms + w.duration_ms)
            .unwrap_or(0);
        let Some(samples) = self.ring.slice(self.sample(start), self.sample(end)) else {
            debug!(paragraph = paragraph_id, "pass 3: audio no longer in ring");
            return;
        };
        self.transcript.paragraphs[idx].pass3_inflight = true;
        self.outstanding_boundary += 1;
        let _ = self.bound_tx.send(Job::Boundary {
            paragraph_id: paragraph_id.to_string(),
            window,
            samples,
        });
    }

    fn on_boundary(
        &mut self,
        paragraph_id: String,
        window: Vec<WindowClip>,
        text: anyhow::Result<String>,
    ) {
        let Some(idx) = self.find_paragraph(&paragraph_id) else {
            self.pass3_pending.remove(&paragraph_id);
            return;
        };
        self.transcript.paragraphs[idx].pass3_inflight = false;
        let plan = match text {
            Ok(t) => boundary::plan_revision(&window, &t).map_err(|r| {
                debug!(paragraph = %paragraph_id, reason = ?r, output = %t, "pass 3 rejected");
            }),
            Err(err) => {
                warn!(err = %format!("{err:#}"), "pass 3 decode failed");
                Err(())
            }
        };
        // Every window clip must still exist and be final.
        let still_valid = window.iter().all(|w| {
            self.find_clip(&w.clip_id)
                .map(|(pi, ci)| !self.transcript.paragraphs[pi].clips[ci].is_partial())
                .unwrap_or(false)
        });
        if let (Ok(split), true) = (plan, still_valid) {
            let soft = self.cfg.paragraph.soft_max_words;
            let p = &mut self.transcript.paragraphs[idx];
            let mut changed = false;
            for (w, new_text) in window.iter().zip(split) {
                if !w.writable {
                    continue;
                }
                if let Some(c) = p.clips.iter_mut().find(|c| c.id == w.clip_id) {
                    changed |= c.text != new_text;
                    c.text = new_text;
                    c.stage = Stage::Revised;
                }
            }
            p.rebuild();
            // Soft cap on pass-3 text, whose trailing punctuation is real
            // rather than SenseVoice's per-clip reflexive period.
            if self.cfg.paragraph.mode == ParagraphMode::Auto
                && !p.closed
                && count_words(&p.text) >= soft
            {
                let ends = p
                    .clips
                    .iter()
                    .rev()
                    .find(|c| c.stage == Stage::Revised)
                    .map(|c| ends_on_sentence(&c.text))
                    .unwrap_or(false);
                if ends {
                    p.closed = true;
                    changed = true;
                }
            }
            if changed {
                self.emit(idx, Change::Revised);
            }
        }
        if self.pass3_pending.remove(&paragraph_id) {
            self.schedule_boundary(&paragraph_id);
        }
    }
}
