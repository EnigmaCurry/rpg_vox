//! `--play NAME`: play NAME.opus and print NAME.ass in time with it,
//! lighting up each word as it is spoken. Without NAME.opus, NAME.md is
//! read aloud instead (see [`crate::speak`]).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context as _, Result};
use ratatui::crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;
use vox_audio::opus_file::{Cursor, OpusFile, RATE};
use vox_audio::playback::Output;
use vox_audio::resample::Linear;
use vox_audio::stretch::Stretch;

use crate::markdown::timestamp;
use crate::speak::Speech;
use crate::speakers;
use crate::tui::{draw_names, NamesMenu, Screen, SpeakerRow};
use crate::RecordPaths;

/// `←` (`↑`) within this long of a word's (line's) start goes to the one
/// before it, so repeated presses keep stepping back while audio plays.
const BACK_GRACE_MS: u64 = 300;
const FRAME: Duration = Duration::from_millis(33);
const GUTTER: usize = 11; // "[hh:mm:ss] "
const SPOKEN: Color = Color::Yellow;

/// One word of a karaoke cue, in ms.
#[derive(Debug, Clone, PartialEq)]
pub struct KWord {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub start_ms: u64,
    pub end_ms: u64,
    pub words: Vec<KWord>,
    /// Speaker from a diarized line's Name ("Speaker A", or a given name).
    pub speaker: Option<String>,
    /// That speaker's colour slot, from the line's style (`S1` → 0).
    pub slot: Option<usize>,
}

/// `h:mm:ss.cc` to ms.
fn ass_clock(s: &str) -> Option<u64> {
    let mut it = s.trim().split(':');
    let (h, m, sc) = (it.next()?, it.next()?, it.next()?);
    let (sec, cs) = sc.split_once('.')?;
    Some(
        ((h.parse::<u64>().ok()? * 60 + m.parse::<u64>().ok()?) * 60 + sec.parse::<u64>().ok()?)
            * 1000
            + cs.parse::<u64>().ok()? * 10,
    )
}

/// Dialogue lines of an ASS file, with per-word times from `\k` tags
/// (`\k`, `\K`, `\kf`, `\ko`). Lines without them spread their words
/// evenly across the cue.
pub fn parse_ass(ass: &str) -> Vec<Cue> {
    let mut cues = Vec::new();
    for line in ass.lines() {
        let Some(rest) = line.strip_prefix("Dialogue:") else {
            continue;
        };
        let fields: Vec<&str> = rest.splitn(10, ',').collect();
        if fields.len() < 10 {
            continue;
        }
        let (Some(start), Some(end)) = (ass_clock(fields[1]), ass_clock(fields[2])) else {
            continue;
        };
        let text = fields[9];
        // (offset in cs from cue start, duration in cs, text)
        let mut segs: Vec<(u64, u64, String)> = Vec::new();
        let mut acc = 0u64;
        let mut timed = false;
        let mut rest = text;
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix('{') {
                let close = after.find('}').map(|i| i + 1).unwrap_or(after.len());
                for tag in after[..close.min(after.len())].split('\\') {
                    let tag = tag.trim_end_matches('}');
                    let num = tag
                        .strip_prefix("kf")
                        .or_else(|| tag.strip_prefix("ko"))
                        .or_else(|| tag.strip_prefix('k'))
                        .or_else(|| tag.strip_prefix('K'));
                    if let Some(cs) = num.and_then(|n| n.parse::<u64>().ok()) {
                        segs.push((acc, cs, String::new()));
                        acc += cs;
                        timed = true;
                    }
                }
                rest = &after[close.min(after.len())..];
                continue;
            }
            let next = rest.find('{').unwrap_or(rest.len());
            let chunk = rest[..next].replace("\\N", " ").replace("\\n", " ");
            match segs.last_mut() {
                Some(seg) => seg.2.push_str(&chunk),
                None => segs.push((0, 0, chunk)),
            }
            rest = &rest[next..];
        }
        let words: Vec<KWord> = if timed {
            segs.into_iter()
                .filter(|s| !s.2.trim().is_empty())
                .map(|(off, dur, t)| KWord {
                    text: t.split_whitespace().collect::<Vec<_>>().join(" "),
                    start_ms: start + off * 10,
                    end_ms: start + (off + dur) * 10,
                })
                .collect()
        } else {
            let all: String = segs.into_iter().map(|s| s.2).collect();
            let ws: Vec<&str> = all.split_whitespace().collect();
            let n = ws.len().max(1) as u64;
            let span = end.saturating_sub(start);
            ws.iter()
                .enumerate()
                .map(|(i, w)| KWord {
                    text: w.to_string(),
                    start_ms: start + span * i as u64 / n,
                    end_ms: start + span * (i as u64 + 1) / n,
                })
                .collect()
        };
        if !words.is_empty() {
            let name = fields[4].trim();
            let speaker = (!name.is_empty()).then(|| name.to_string());
            let slot = fields[3]
                .trim()
                .strip_prefix('S')
                .and_then(|n| n.parse::<usize>().ok())
                .and_then(|n| n.checked_sub(1));
            cues.push(Cue {
                start_ms: start,
                end_ms: end,
                words,
                speaker,
                slot,
            });
        }
    }
    cues.sort_by_key(|c| c.start_ms);
    cues
}

/// Where the audio comes from.
enum Track<'a> {
    Recording(&'a OpusFile),
    /// Speech still being made, read as it grows.
    Speech(Arc<Speech>),
}

impl<'a> Track<'a> {
    fn rate(&self) -> u32 {
        match self {
            Track::Recording(_) => RATE,
            Track::Speech(s) => s.rate,
        }
    }

    fn reader(&self, pos: u64) -> Result<Reader<'a>> {
        Ok(match self {
            Track::Recording(f) => Reader::Recording(f.cursor(pos)?),
            Track::Speech(s) => Reader::Speech {
                speech: s.clone(),
                pos: (pos as usize).min(s.len()),
            },
        })
    }
}

enum Reader<'a> {
    Recording(Cursor<'a>),
    Speech { speech: Arc<Speech>, pos: usize },
}

impl Reader<'_> {
    /// Append the next chunk to `out`; `false` at the end. While speech
    /// is still being made, appends nothing and returns `true`.
    fn read(&mut self, out: &mut Vec<f32>) -> Result<bool> {
        match self {
            Reader::Recording(c) => c.read(out),
            Reader::Speech { speech, pos } => Ok(match speech.read(*pos, 2400, out) {
                Some(n) => {
                    *pos += n;
                    true
                }
                None => false,
            }),
        }
    }
}

/// Feeds decoded audio to the output and keeps the clock.
struct Player<'a> {
    track: Track<'a>,
    rate: u32,
    cursor: Reader<'a>,
    out: Output,
    resampler: Linear,
    /// Speed changes without changing pitch.
    stretch: Stretch,
    /// Resampled audio not yet accepted by the output.
    buf: Vec<f32>,
    decoded: Vec<f32>,
    stretched: Vec<f32>,
    /// Position the output's played counter counts from.
    base_ms: u64,
    eof: bool,
    paused: bool,
}

impl<'a> Player<'a> {
    fn new(track: Track<'a>) -> Result<Self> {
        let out = Output::open().context("open audio output")?;
        let rate = track.rate();
        Ok(Self {
            cursor: track.reader(0)?,
            track,
            rate,
            resampler: Linear::new(rate, out.sample_rate),
            stretch: Stretch::new(rate),
            out,
            buf: Vec::new(),
            decoded: Vec::new(),
            stretched: Vec::new(),
            base_ms: 0,
            eof: false,
            paused: false,
        })
    }

    fn len_ms(&self) -> u64 {
        let len = match &self.track {
            Track::Recording(f) => f.len,
            Track::Speech(s) => s.len() as u64,
        };
        len * 1000 / self.rate as u64
    }

    /// Caught up with speech that is still being made.
    fn waiting(&self) -> bool {
        matches!(&self.cursor, Reader::Speech { speech, pos } if !speech.done() && *pos >= speech.len())
            && self.out.queued() == 0
    }

    /// Each second played covers `speed` seconds of the recording.
    fn position_ms(&self) -> u64 {
        let played = self.out.played() as f64 * 1000.0 / self.out.sample_rate as f64;
        (self.base_ms + (played * self.stretch.speed()) as u64).min(self.len_ms())
    }

    fn speed(&self) -> f64 {
        self.stretch.speed()
    }

    /// Play at `speed`× from where playback is now.
    fn set_speed(&mut self, speed: f64) -> Result<()> {
        let pos = self.position_ms();
        self.stretch.set_speed(speed);
        self.seek(pos)
    }

    fn ended(&self) -> bool {
        self.eof && self.buf.is_empty() && self.out.queued() == 0
    }

    /// Top up the output ring.
    fn fill(&mut self) -> Result<()> {
        while self.out.free() > 0 {
            if self.buf.is_empty() {
                if self.eof {
                    return Ok(());
                }
                self.decoded.clear();
                self.stretched.clear();
                if !self.cursor.read(&mut self.decoded)? {
                    self.eof = true;
                    self.stretch.flush(&mut self.stretched);
                    self.resampler.process(&self.stretched, &mut self.buf);
                    continue;
                }
                self.stretch.process(&self.decoded, &mut self.stretched);
                self.resampler.process(&self.stretched, &mut self.buf);
            }
            let n = self.out.push(&self.buf);
            self.buf.drain(..n);
            if n == 0 {
                return Ok(());
            }
        }
        Ok(())
    }

    fn seek(&mut self, ms: u64) -> Result<()> {
        let ms = ms.min(self.len_ms());
        self.out.flush();
        self.cursor = self.track.reader(ms * self.rate as u64 / 1000)?;
        self.resampler.reset();
        self.stretch.reset();
        self.buf.clear();
        self.base_ms = ms;
        self.eof = false;
        Ok(())
    }

    fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        self.out.set_paused(paused);
    }
}

/// Play the recording NAME.opus with NAME.ass.
pub fn run(paths: &RecordPaths) -> Result<()> {
    if !paths.opus.exists() {
        bail!(
            "{} not found (record one with --record)",
            paths.opus.display()
        );
    }
    let ass = std::fs::read_to_string(&paths.ass)
        .with_context(|| format!("read {}", paths.ass.display()))?;
    let file = OpusFile::open(&paths.opus)?;
    let screen = Screen::enter(" opening audio…");
    play(paths, screen, Track::Recording(&file), parse_ass(&ass))
}

/// Read NAME.md aloud with the Kokoro model in `model_dir`; `voices`
/// name the voice of each speaker in turn.
pub fn run_speech(paths: &RecordPaths, model_dir: &Path, voices: &[String]) -> Result<()> {
    let md = std::fs::read_to_string(&paths.md)
        .with_context(|| format!("read {}", paths.md.display()))?;
    let screen = Screen::enter(" loading the voice model…");
    let speech = crate::speak::start(&md, model_dir, voices)?;
    let result = play(paths, screen, Track::Speech(speech.clone()), Vec::new());
    speech.stop();
    result
}

fn play(paths: &RecordPaths, mut screen: Screen, track: Track, mut cues: Vec<Cue>) -> Result<()> {
    let speech = match &track {
        Track::Speech(s) => Some(s.clone()),
        Track::Recording(_) => None,
    };
    let mut blocks = sentences(&cues);
    let mut starts = word_starts(&cues);
    let mut line_starts: Vec<u64> = cues.iter().map(|c| c.start_ms).collect();
    let mut search = Search::default();
    // Speaker renames: name in the file as opened → name now.
    let mut renamed: HashMap<String, String> = HashMap::new();
    let mut names_menu: Option<NamesMenu> = None;
    let mut status: Option<(String, Instant)> = None;
    let mut name = paths
        .md
        .with_extension("")
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if speech.is_some() {
        name.push_str(" · read aloud");
    }
    let mut speech_error = false;

    let mut player = Player::new(track)?;
    let mut last_draw = Instant::now() - FRAME;
    loop {
        if let Some(s) = &speech {
            let new = s.cues_from(cues.len());
            if !new.is_empty() {
                cues.extend(new);
                blocks = sentences(&cues);
                starts = word_starts(&cues);
                line_starts = cues.iter().map(|c| c.start_ms).collect();
            }
            s.set_playhead(player.position_ms());
            if let Some(e) = s.error().filter(|_| !speech_error) {
                speech_error = true;
                status = Some((format!("speech stopped: {e}"), Instant::now()));
            }
        }
        player.fill()?;
        if player.ended() && !player.paused {
            player.set_paused(true);
        }
        if last_draw.elapsed() >= FRAME {
            let pos = if player.ended() {
                player.len_ms()
            } else {
                player.position_ms()
            };
            screen.terminal().draw(|f| {
                let view = View {
                    cues: &cues,
                    blocks: &blocks,
                    player: &player,
                    search: &search,
                    renamed: &renamed,
                    speaking: speech
                        .as_ref()
                        .filter(|s| !s.done())
                        .map(|s| (cues.len(), s.total)),
                    status: status
                        .as_ref()
                        .filter(|(_, at)| at.elapsed() < STATUS_TTL)
                        .map(|(m, _)| m.as_str()),
                };
                draw(f, &name, &view, pos);
                if let Some(menu) = &names_menu {
                    draw_names(f, f.area(), menu, &speaker_rows(&cues, &renamed));
                }
            })?;
            last_draw = Instant::now();
        }
        if !event::poll(Duration::from_millis(10))? {
            continue;
        }
        let Some(key) = (match event::read()? {
            TermEvent::Key(k) => Some(crate::tui::normalize_key(k)),
            _ => None,
        }) else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        let pos = player.position_ms();
        let words: Vec<&KWord> = cues.iter().flat_map(|c| c.words.iter()).collect();
        if let Some(menu) = names_menu.as_mut() {
            let rows = speaker_rows(&cues, &renamed);
            menu.selected = menu.selected.min(rows.len().saturating_sub(1));
            let row = &rows[menu.selected];
            if let Some(buf) = menu.editing.as_mut() {
                match key.code {
                    KeyCode::Enter => {
                        let new = buf.clone();
                        menu.editing = None;
                        let msg = rename(paths, &cues, &mut renamed, &row.name, &new, &rows)
                            .unwrap_or_else(|e| format!("rename failed: {e:#}"));
                        status = Some((msg, Instant::now()));
                    }
                    KeyCode::Esc => menu.editing = None,
                    KeyCode::Backspace => {
                        buf.pop();
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                    KeyCode::Char(c) => buf.push(c),
                    _ => {}
                }
            } else {
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        menu.selected = menu.selected.saturating_sub(1)
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        menu.selected = (menu.selected + 1).min(rows.len() - 1)
                    }
                    KeyCode::Enter => menu.editing = Some(String::new()),
                    KeyCode::Delete | KeyCode::Backspace => {
                        let default = format!("Speaker {}", row.tag);
                        let msg = rename(paths, &cues, &mut renamed, &row.name, &default, &rows)
                            .unwrap_or_else(|e| format!("rename failed: {e:#}"));
                        status = Some((msg, Instant::now()));
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                    KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('q') => names_menu = None,
                    _ => {}
                }
            }
            last_draw = Instant::now() - FRAME;
            continue;
        }
        if let Some(typed) = search.prompt.as_mut() {
            match key.code {
                KeyCode::Enter => {
                    let pattern = search.prompt.take().unwrap_or_default();
                    if let Some(t) = search.start(&pattern, &words, pos) {
                        player.seek(t)?;
                    }
                }
                KeyCode::Esc => search.prompt = None,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    search.prompt = None
                }
                KeyCode::Backspace => {
                    if typed.pop().is_none() {
                        search.prompt = None;
                    }
                }
                KeyCode::Char(c) => typed.push(c),
                _ => {}
            }
            last_draw = Instant::now() - FRAME;
            continue;
        }
        match key.code {
            KeyCode::Char('/') => {
                search.prompt = Some(String::new());
                search.message = None;
            }
            // `n` steps through search matches while there is a search;
            // otherwise it opens the speaker list, as in the recording TUI.
            KeyCode::Char('n') if search.active() => {
                if let Some(t) = search.step(1, &words) {
                    player.seek(t)?;
                }
            }
            KeyCode::Char('n') => {
                if speaker_rows(&cues, &renamed).is_empty() {
                    status = Some(("no speakers in this recording".into(), Instant::now()));
                } else {
                    names_menu = Some(NamesMenu {
                        selected: 0,
                        editing: None,
                    });
                }
            }
            KeyCode::Char('p') | KeyCode::Char('N') => {
                if let Some(t) = search.step(-1, &words) {
                    player.seek(t)?;
                }
            }
            KeyCode::Esc if search.active() => search = Search::default(),
            KeyCode::Char('q') | KeyCode::Esc => break,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
            KeyCode::Char(' ') => {
                if player.ended() {
                    player.seek(0)?;
                    player.set_paused(false);
                } else {
                    player.set_paused(!player.paused);
                }
            }
            KeyCode::Left => player.seek(prev_start(&starts, pos))?,
            KeyCode::Right => {
                if let Some(t) = next_start(&starts, pos) {
                    player.seek(t)?;
                }
            }
            KeyCode::Up => player.seek(prev_start(&line_starts, pos))?,
            KeyCode::Down => {
                if let Some(t) = next_start(&line_starts, pos) {
                    player.seek(t)?;
                }
            }
            KeyCode::Home => player.seek(0)?,
            KeyCode::Char('<') | KeyCode::Char(',') => {
                player.set_speed(step_speed(player.speed(), -1))?
            }
            KeyCode::Char('>') | KeyCode::Char('.') => {
                player.set_speed(step_speed(player.speed(), 1))?
            }
            _ => {}
        }
        last_draw = Instant::now() - FRAME;
    }
    screen.restore();
    Ok(())
}

/// Playback speeds `<` and `>` step through.
const SPEEDS: [f64; 9] = [0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];

/// The next speed up (`dir` 1) or down (-1) from `now`.
fn step_speed(now: f64, dir: i32) -> f64 {
    let i = SPEEDS
        .iter()
        .position(|&s| (s - now).abs() < 1e-9)
        .unwrap_or(2) as i32;
    SPEEDS[(i + dir).clamp(0, SPEEDS.len() as i32 - 1) as usize]
}

/// `/` search over the transcript, like `less`: case-insensitive, and
/// a match may span several words.
#[derive(Default)]
struct Search {
    /// Pattern being typed after `/`.
    prompt: Option<String>,
    pattern: String,
    /// Matches as (first, last) index into the flat word list.
    hits: Vec<(usize, usize)>,
    current: usize,
    /// "not found", "search wrapped", …
    message: Option<String>,
}

impl Search {
    fn active(&self) -> bool {
        !self.pattern.is_empty()
    }

    /// Search for `pattern`; returns where the first match at or after
    /// `pos` starts (wrapping to the top).
    fn start(&mut self, pattern: &str, words: &[&KWord], pos: u64) -> Option<u64> {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            // A bare `/` repeats the last search, as in less.
            return self.step(1, words);
        }
        self.pattern = pattern.to_string();
        self.hits = find(words, pattern);
        if self.hits.is_empty() {
            self.message = Some("pattern not found".into());
            return None;
        }
        match self.hits.iter().position(|h| words[h.0].start_ms >= pos) {
            Some(i) => {
                self.current = i;
                self.message = None;
            }
            None => {
                self.current = 0;
                self.message = Some("search wrapped".into());
            }
        }
        Some(words[self.hits[self.current].0].start_ms)
    }

    /// Move `dir` (+1 / -1) matches, wrapping around the ends.
    fn step(&mut self, dir: isize, words: &[&KWord]) -> Option<u64> {
        if self.hits.is_empty() {
            if self.active() {
                self.message = Some("pattern not found".into());
            }
            return None;
        }
        let n = self.hits.len() as isize;
        let next = self.current as isize + dir;
        self.message = (next < 0 || next >= n).then(|| "search wrapped".into());
        self.current = next.rem_euclid(n) as usize;
        Some(words[self.hits[self.current].0].start_ms)
    }

    /// 0 = not a match, 1 = another match, 2 = the current match.
    fn mark(&self, word: usize) -> u8 {
        let i = self.hits.partition_point(|h| h.1 < word);
        match self.hits.get(i) {
            Some(h) if h.0 <= word => {
                if i == self.current {
                    2
                } else {
                    1
                }
            }
            _ => 0,
        }
    }
}

/// Case-insensitive matches of `pattern` in the words joined by
/// spaces, as (first, last) word indices.
fn find(words: &[&KWord], pattern: &str) -> Vec<(usize, usize)> {
    let pattern = pattern
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let mut text = String::new();
    // Byte offset where each word starts in `text`.
    let mut at = Vec::with_capacity(words.len());
    for w in words {
        if !text.is_empty() {
            text.push(' ');
        }
        at.push(text.len());
        text.push_str(&w.text.to_lowercase());
    }
    let word_of = |byte: usize| at.partition_point(|&a| a <= byte).saturating_sub(1);
    text.match_indices(&pattern)
        .map(|(i, m)| (word_of(i), word_of(i + m.len().saturating_sub(1))))
        .collect()
}

/// Every word's start time, ascending.
fn word_starts(cues: &[Cue]) -> Vec<u64> {
    let mut s: Vec<u64> = cues
        .iter()
        .flat_map(|c| c.words.iter().map(|w| w.start_ms))
        .collect();
    s.sort_unstable();
    s.dedup();
    s
}

/// First of `starts` (word or line starts, ascending) after `pos`.
fn next_start(starts: &[u64], pos: u64) -> Option<u64> {
    starts.get(starts.partition_point(|&s| s <= pos)).copied()
}

/// Start of the word (line) playing at `pos`, or of the one before it
/// when `pos` is just past its start (0 before the first).
fn prev_start(starts: &[u64], pos: u64) -> u64 {
    let Some(cur) = starts.partition_point(|&s| s <= pos).checked_sub(1) else {
        return 0;
    };
    let i = if pos - starts[cur] < BACK_GRACE_MS {
        cur.checked_sub(1)
    } else {
        Some(cur)
    };
    i.map(|i| starts[i]).unwrap_or(0)
}

fn clock(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

/// A pause this long between cues ends a sentence block even without
/// a full stop.
const BLOCK_GAP_MS: u64 = 2000;

/// Cue ranges shown together: a subtitle cue holds at most two short
/// lines, so a sentence often spans several. A block runs until a cue
/// ends a sentence, the speaker changes, or there is a long pause.
fn sentences(cues: &[Cue]) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 0..cues.len() {
        let ends = cues[i].words.last().is_some_and(|w| {
            w.text
                .trim_end_matches(['"', '\'', ')'])
                .ends_with(['.', '?', '!'])
        });
        let next_breaks = cues.get(i + 1).is_none_or(|n| {
            n.speaker != cues[i].speaker
                || n.start_ms.saturating_sub(cues[i].end_ms) >= BLOCK_GAP_MS
        });
        if ends || next_breaks {
            out.push(start..i + 1);
            start = i + 1;
        }
    }
    out
}

/// What [`draw`] shows besides the clock.
struct View<'a, 'b> {
    cues: &'a [Cue],
    blocks: &'a [std::ops::Range<usize>],
    player: &'a Player<'b>,
    search: &'a Search,
    renamed: &'a HashMap<String, String>,
    /// Reading aloud: sentences spoken so far, of how many.
    speaking: Option<(usize, usize)>,
    status: Option<&'a str>,
}

/// How long a footer message stays.
const STATUS_TTL: Duration = Duration::from_secs(3);

/// The speakers in `cues` (as renamed), in colour-slot order.
fn speaker_rows(cues: &[Cue], renamed: &HashMap<String, String>) -> Vec<SpeakerRow> {
    let mut seen: Vec<(usize, String)> = Vec::new();
    for c in cues {
        if let Some(s) = &c.speaker {
            let name = renamed.get(s).cloned().unwrap_or_else(|| s.clone());
            if !seen.iter().any(|(_, n)| *n == name) {
                seen.push((c.slot.unwrap_or(usize::MAX), name));
            }
        }
    }
    seen.sort_by_key(|(slot, _)| *slot);
    seen.into_iter()
        .map(|(slot, name)| SpeakerRow {
            tag: if slot == usize::MAX {
                "?".into()
            } else {
                vox_transcribe::speaker::label(slot)
            },
            color: if slot == usize::MAX {
                Color::Gray
            } else {
                speakers::color_at(slot)
            },
            name,
        })
        .collect()
}

/// Rename speaker `old` (its current name) to `new` on screen and in the
/// recording's markdown, SRT and ASS. Returns a message for the footer.
fn rename(
    paths: &RecordPaths,
    cues: &[Cue],
    renamed: &mut HashMap<String, String>,
    old: &str,
    new: &str,
    rows: &[SpeakerRow],
) -> Result<String> {
    let new = new
        .replace(',', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if new.is_empty() || new == old {
        return Ok(format!("{old}: unchanged"));
    }
    if rows.iter().any(|r| r.name == new) {
        return Ok(format!("{new:?} is already another speaker"));
    }
    for (path, kind) in [
        (&paths.ass, Kind::Ass),
        (&paths.srt, Kind::Srt),
        (&paths.md, Kind::Md),
    ] {
        if let Ok(text) = std::fs::read_to_string(path) {
            replace_file(path, &rename_in(&text, kind, old, &new))?;
        }
    }
    for c in cues {
        if let Some(s) = &c.speaker {
            let now = renamed.get(s).cloned().unwrap_or_else(|| s.clone());
            if now == old {
                renamed.insert(s.clone(), new.clone());
            }
        }
    }
    Ok(format!("{old} is now {new}"))
}

#[derive(Clone, Copy)]
enum Kind {
    Ass,
    Srt,
    Md,
}

/// `text` (a recording's ASS, SRT or markdown) with speaker `old` named
/// `new`: the ASS Name field, an SRT cue's `old: ` prefix, a markdown
/// paragraph's `**[ts] old:**`.
fn rename_in(text: &str, kind: Kind, old: &str, new: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut srt_first_text = false;
    for line in text.split_inclusive('\n') {
        let replaced = match kind {
            Kind::Ass => line.strip_prefix("Dialogue:").and_then(|rest| {
                let f: Vec<&str> = rest.splitn(10, ',').collect();
                (f.len() == 10 && f[4] == old).then(|| {
                    let mut g = f.clone();
                    g[4] = new;
                    format!("Dialogue:{}", g.join(","))
                })
            }),
            Kind::Srt => {
                let r = if srt_first_text {
                    line.strip_prefix(&format!("{old}: "))
                        .map(|t| format!("{new}: {t}"))
                } else {
                    None
                };
                srt_first_text = line.contains(" --> ");
                r
            }
            Kind::Md => line
                .strip_prefix("**[")
                .and_then(|r| r.split_once("] "))
                .and_then(|(ts, r)| {
                    r.strip_prefix(&format!("{old}:** "))
                        .map(|t| format!("**[{ts}] {new}:** {t}"))
                }),
        };
        out.push_str(replaced.as_deref().unwrap_or(line));
    }
    out
}

/// Write `content` to `path` via a temporary file and a rename.
fn replace_file(path: &std::path::Path, content: &str) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    std::fs::write(&tmp, content).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))
}

fn draw(f: &mut Frame, name: &str, view: &View, pos: u64) {
    let View {
        cues,
        blocks,
        player,
        search,
        renamed,
        speaking,
        status,
    } = *view;
    let shown = |s: &str| renamed.get(s).cloned().unwrap_or_else(|| s.to_string());
    let [main, bar, keys] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(f.area());

    let block = Block::bordered().title(format!(" {name} "));
    let inner = block.inner(main);
    let width = (inner.width as usize).max(GUTTER + 10);
    let mut lines: Vec<Line> = Vec::new();
    let mut index = 0;
    let mut last_speaker = None;
    // A whole sentence appears once it starts, the words still to come
    // dimmed, so it can be read ahead.
    for block in blocks.iter().take_while(|b| cues[b.start].start_ms <= pos) {
        let first = &cues[block.start];
        let mut spans: Vec<Span> = Vec::new();
        // Name the speaker whenever it changes.
        if let Some(s) = first
            .speaker
            .as_deref()
            .filter(|&s| last_speaker != Some(s))
        {
            let color = first.slot.map(speakers::color_at).unwrap_or(Color::Gray);
            spans.extend(label_spans(&shown(s), Style::new().fg(color).bold()));
        }
        last_speaker = first.speaker.as_deref();
        spans.extend(cues[block.clone()].iter().flat_map(|c| &c.words).map(|w| {
            let style = if w.end_ms <= pos {
                Style::new()
            } else if w.start_ms <= pos {
                Style::new().fg(SPOKEN).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(Color::DarkGray)
            };
            let style = match search.mark(index) {
                2 => style.fg(Color::Black).bg(Color::Cyan),
                1 => style.add_modifier(Modifier::UNDERLINED),
                _ => style,
            };
            index += 1;
            Span::styled(w.text.clone(), style)
        }));
        let gutter = Span::styled(
            format!("[{}] ", timestamp(first.start_ms)),
            Style::new().fg(Color::DarkGray),
        );
        lines.extend(wrap(gutter, spans, width));
    }
    let height = inner.height as usize;
    let skip = lines.len().saturating_sub(height);
    let lines: Vec<Line> = lines.into_iter().skip(skip).collect();
    f.render_widget(Paragraph::new(lines).block(block), main);

    let state = if player.ended() {
        Span::styled(" END ", Style::new().black().on_dark_gray())
    } else if player.waiting() && !player.paused {
        Span::styled(" … VOICING ", Style::new().black().on_cyan())
    } else if player.paused {
        Span::styled(" ❚❚ PAUSED ", Style::new().black().on_yellow())
    } else {
        Span::styled(" ▶ PLAYING ", Style::new().black().on_green())
    };
    let speed = {
        let x = format!("{:.2}", player.speed());
        let x = x.trim_end_matches('0').trim_end_matches('.');
        let style = if (player.speed() - 1.0).abs() < 1e-9 {
            Style::new().dark_gray()
        } else {
            Style::new().black().on_cyan()
        };
        Span::styled(format!(" {x}x "), style)
    };
    let len = player.len_ms().max(1);
    let label = match speaking {
        Some((done, total)) => {
            format!(" {} / {}+ ({done}/{total} voiced) ", clock(pos), clock(len))
        }
        None => format!(" {} / {} ", clock(pos), clock(len)),
    };
    let room =
        (bar.width as usize).saturating_sub(state.width() + speed.width() + label.width() + 1);
    let filled = (room as u64 * pos.min(len) / len) as usize;
    f.render_widget(
        Line::from(vec![
            state,
            speed,
            Span::raw(label),
            Span::styled("━".repeat(filled), Style::new().fg(SPOKEN)),
            Span::styled("─".repeat(room - filled), Style::new().fg(Color::DarkGray)),
        ]),
        bar,
    );
    let footer = if let Some(msg) = status {
        Line::from(format!(" {msg}")).yellow()
    } else if let Some(typed) = &search.prompt {
        Line::from(format!("/{typed}▏"))
    } else if search.active() {
        let count = if search.hits.is_empty() {
            String::new()
        } else {
            format!("  {}/{}", search.current + 1, search.hits.len())
        };
        let note = search
            .message
            .as_deref()
            .map(|m| format!("  ({m})"))
            .unwrap_or_default();
        Line::from(vec![
            Span::raw(format!(" /{}", search.pattern)),
            Span::styled(format!("{count}{note}"), Style::new().fg(Color::Cyan)),
            Span::raw("  n next · p previous · esc clear").dark_gray(),
        ])
    } else {
        Line::from(
            " space pause · ←/→ word · ↑/↓ line · < > speed · / search · n speakers · home restart · q quit",
        )
        .dark_gray()
    };
    f.render_widget(footer, keys);
}

/// "Alice Smith:" as one span per word, so it wraps like the text.
pub fn label_spans(name: &str, style: Style) -> Vec<Span<'static>> {
    let mut parts: Vec<String> = name.split_whitespace().map(String::from).collect();
    if let Some(last) = parts.last_mut() {
        last.push(':');
    }
    parts.into_iter().map(|p| Span::styled(p, style)).collect()
}

/// Word-wrap one cue to `width`, indenting continuation lines under the
/// gutter.
fn wrap<'a>(gutter: Span<'a>, words: Vec<Span<'a>>, width: usize) -> Vec<Line<'a>> {
    let mut lines = Vec::new();
    let mut cur: Vec<Span> = vec![gutter];
    let mut used = GUTTER;
    for w in words {
        let len = w.content.width();
        if used > GUTTER && used + 1 + len > width {
            lines.push(Line::from(std::mem::take(&mut cur)));
            cur.push(Span::raw(" ".repeat(GUTTER)));
            used = GUTTER;
        }
        if used > GUTTER {
            cur.push(Span::raw(" "));
            used += 1;
        }
        used += len;
        cur.push(w);
    }
    lines.push(Line::from(cur));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_karaoke_lines() {
        let ass = "[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
            Dialogue: 0,0:00:01.00,0:00:02.50,Default,,0,0,0,,{\\k20}Hi,\\N{\\k30}{\\k40}there, you\n";
        let cues = parse_ass(ass);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].speaker, None);
        assert_eq!(
            cues[0].words,
            vec![
                KWord {
                    text: "Hi,".into(),
                    start_ms: 1000,
                    end_ms: 1200
                },
                KWord {
                    text: "there, you".into(),
                    start_ms: 1500,
                    end_ms: 1900
                },
            ]
        );
    }

    #[test]
    fn steps_between_words() {
        let starts = [1000, 1500, 4000];
        assert_eq!(next_start(&starts, 0), Some(1000));
        assert_eq!(next_start(&starts, 1000), Some(1500));
        assert_eq!(next_start(&starts, 2000), Some(4000));
        assert_eq!(next_start(&starts, 4000), None);
        // Mid-word: back to that word's start.
        assert_eq!(prev_start(&starts, 3000), 1500);
        // Just after a start: the word before.
        assert_eq!(prev_start(&starts, 1600), 1000);
        assert_eq!(prev_start(&starts, 1100), 0);
        // Close-together words: only the current one is skipped.
        assert_eq!(prev_start(&[0, 160, 560, 800], 800), 560);
    }

    #[test]
    fn searches_across_words() {
        let ws: Vec<KWord> = ["The", "Goblin", "King,", "the", "goblin", "fled."]
            .iter()
            .enumerate()
            .map(|(i, t)| KWord {
                text: t.to_string(),
                start_ms: i as u64 * 100,
                end_ms: i as u64 * 100 + 90,
            })
            .collect();
        let words: Vec<&KWord> = ws.iter().collect();
        assert_eq!(find(&words, "goblin  king"), vec![(1, 2)]);
        assert_eq!(find(&words, "GOB"), vec![(1, 1), (4, 4)]);
        assert!(find(&words, "dragon").is_empty());

        let mut s = Search::default();
        // From the middle: the next match, then wrap back to the first.
        assert_eq!(s.start("goblin", &words, 250), Some(400));
        assert_eq!(s.step(1, &words), Some(100));
        assert_eq!(s.message.as_deref(), Some("search wrapped"));
        assert_eq!(s.step(-1, &words), Some(400));
        assert_eq!((s.mark(4), s.mark(1), s.mark(0)), (2, 1, 0));
        assert_eq!(s.start("dragon", &words, 0), None);
        assert_eq!(s.message.as_deref(), Some("pattern not found"));
    }

    #[test]
    fn groups_cues_into_sentences() {
        let cue = |s: u64, e: u64, text: &str, who: Option<&str>| Cue {
            start_ms: s,
            end_ms: e,
            words: vec![KWord {
                text: text.into(),
                start_ms: s,
                end_ms: e,
            }],
            speaker: who.map(String::from),
            slot: None,
        };
        let cues = [
            cue(0, 1000, "It is a collection", None),
            cue(1000, 2000, "of tools.", None),
            cue(2000, 3000, "Next", None),
            cue(6000, 7000, "after a pause", None),
            cue(7000, 8000, "and on", Some("B")),
        ];
        assert_eq!(sentences(&cues), vec![0..2, 2..3, 3..4, 4..5]);
    }

    #[test]
    fn speed_steps_and_stops_at_the_ends() {
        assert_eq!(step_speed(1.0, 1), 1.25);
        assert_eq!(step_speed(1.0, -1), 0.75);
        assert_eq!(step_speed(3.0, 1), 3.0);
        assert_eq!(step_speed(0.5, -1), 0.5);
    }

    #[test]
    fn renames_in_each_format() {
        let ass = "[Events]\nDialogue: 0,0:00:00.00,0:00:02.00,S1,Speaker A,0,0,0,,{\\k20}Hi, A\nDialogue: 0,0:00:02.00,0:00:03.00,S2,Speaker B,0,0,0,,Yo\n";
        let out = rename_in(ass, Kind::Ass, "Speaker A", "Sam");
        assert!(out.contains(",S1,Sam,0,0,0,,{\\k20}Hi, A\n"));
        assert!(out.contains(",S2,Speaker B,"));
        let srt = "1\n00:00:00,000 --> 00:00:02,000\nSpeaker A: Hi.\n\n2\n00:00:02,000 --> 00:00:03,000\nSpeaker A: is me\nSpeaker A: not a prefix line\n\n";
        let out = rename_in(srt, Kind::Srt, "Speaker A", "Sam");
        assert_eq!(out.matches("Sam: ").count(), 2);
        assert!(out.contains("\nSpeaker A: not a prefix line"));
        let md =
            "# T\n\n**[00:00:00] Speaker A:** Hi.\n\n**[00:00:05] Speaker B:** Speaker A: no.\n";
        let out = rename_in(md, Kind::Md, "Speaker A", "Sam");
        assert!(out.contains("**[00:00:00] Sam:** Hi."));
        assert!(out.contains("**[00:00:05] Speaker B:** Speaker A: no."));
    }

    #[test]
    fn reads_speaker_from_name() {
        let ass = "Dialogue: 0,0:00:00.00,0:00:02.00,S2,Speaker B,0,0,0,,{\\k20}Hi\n";
        let cue = &parse_ass(ass)[0];
        assert_eq!(cue.speaker.as_deref(), Some("Speaker B"));
        assert_eq!(cue.slot, Some(1));
    }

    #[test]
    fn spreads_untimed_lines() {
        let ass = "Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,a b\n";
        let cues = parse_ass(ass);
        assert_eq!(cues[0].words[1].start_ms, 1000);
    }
}
