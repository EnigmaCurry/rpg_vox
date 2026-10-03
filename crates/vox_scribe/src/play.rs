//! `--play NAME`: play NAME.opus and print NAME.ass in time with it,
//! lighting up each word as it is spoken.

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

use crate::markdown::timestamp;
use crate::speakers;
use crate::tui::Screen;
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

/// Feeds decoded audio to the output and keeps the clock.
struct Player<'a> {
    file: &'a OpusFile,
    cursor: Cursor<'a>,
    out: Output,
    resampler: Linear,
    /// Resampled audio not yet accepted by the output.
    buf: Vec<f32>,
    decoded: Vec<f32>,
    /// Position the output's played counter counts from.
    base_ms: u64,
    eof: bool,
    paused: bool,
}

impl<'a> Player<'a> {
    fn new(file: &'a OpusFile) -> Result<Self> {
        let out = Output::open().context("open audio output")?;
        Ok(Self {
            file,
            cursor: file.cursor(0)?,
            resampler: Linear::new(RATE, out.sample_rate),
            out,
            buf: Vec::new(),
            decoded: Vec::new(),
            base_ms: 0,
            eof: false,
            paused: false,
        })
    }

    fn len_ms(&self) -> u64 {
        self.file.len * 1000 / RATE as u64
    }

    fn position_ms(&self) -> u64 {
        (self.base_ms + self.out.played() * 1000 / self.out.sample_rate as u64).min(self.len_ms())
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
                if !self.cursor.read(&mut self.decoded)? {
                    self.eof = true;
                    return Ok(());
                }
                self.resampler.process(&self.decoded, &mut self.buf);
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
        self.cursor = self.file.cursor(ms * RATE as u64 / 1000)?;
        self.resampler.reset();
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

pub fn run(paths: &RecordPaths) -> Result<()> {
    if !paths.opus.exists() {
        bail!(
            "{} not found (record one with --record)",
            paths.opus.display()
        );
    }
    let ass = std::fs::read_to_string(&paths.ass)
        .with_context(|| format!("read {}", paths.ass.display()))?;
    let cues = parse_ass(&ass);
    let blocks = sentences(&cues);
    let starts = word_starts(&cues);
    let line_starts: Vec<u64> = cues.iter().map(|c| c.start_ms).collect();
    let words: Vec<&KWord> = cues.iter().flat_map(|c| c.words.iter()).collect();
    let mut search = Search::default();
    let file = OpusFile::open(&paths.opus)?;
    let name = paths
        .md
        .with_extension("")
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut screen = Screen::enter(" opening audio…");
    let mut player = Player::new(&file)?;
    let mut last_draw = Instant::now() - FRAME;
    loop {
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
            screen
                .terminal()
                .draw(|f| draw(f, &name, &cues, &blocks, &player, &search, pos))?;
            last_draw = Instant::now();
        }
        if !event::poll(Duration::from_millis(10))? {
            continue;
        }
        let TermEvent::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        let pos = player.position_ms();
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
            KeyCode::Char('n') => {
                if let Some(t) = search.step(1, &words) {
                    player.seek(t)?;
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
            _ => {}
        }
        last_draw = Instant::now() - FRAME;
    }
    screen.restore();
    Ok(())
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

fn draw(
    f: &mut Frame,
    name: &str,
    cues: &[Cue],
    blocks: &[std::ops::Range<usize>],
    player: &Player,
    search: &Search,
    pos: u64,
) {
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
            spans.extend(label_spans(s, Style::new().fg(color).bold()));
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
    } else if player.paused {
        Span::styled(" ❚❚ PAUSED ", Style::new().black().on_yellow())
    } else {
        Span::styled(" ▶ PLAYING ", Style::new().black().on_green())
    };
    let len = player.len_ms().max(1);
    let label = format!(" {} / {} ", clock(pos), clock(len));
    let room = (bar.width as usize).saturating_sub(state.width() + label.width() + 1);
    let filled = (room as u64 * pos.min(len) / len) as usize;
    f.render_widget(
        Line::from(vec![
            state,
            Span::raw(label),
            Span::styled("━".repeat(filled), Style::new().fg(SPOKEN)),
            Span::styled("─".repeat(room - filled), Style::new().fg(Color::DarkGray)),
        ]),
        bar,
    );
    let footer = if let Some(typed) = &search.prompt {
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
        Line::from(" space pause · ←/→ word · ↑/↓ line · / search · home restart · q quit")
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
