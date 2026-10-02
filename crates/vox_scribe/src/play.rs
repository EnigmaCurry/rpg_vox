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
use crate::tui::Screen;
use crate::RecordPaths;

const SKIP_MS: u64 = 5000;
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
            cues.push(Cue {
                start_ms: start,
                end_ms: end,
                words,
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
                .draw(|f| draw(f, &name, &cues, &player, pos))?;
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
        match key.code {
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
            KeyCode::Left => player.seek(pos.saturating_sub(SKIP_MS))?,
            KeyCode::Right => player.seek(pos + SKIP_MS)?,
            KeyCode::Home => player.seek(0)?,
            _ => {}
        }
        last_draw = Instant::now() - FRAME;
    }
    screen.restore();
    Ok(())
}

fn clock(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

fn draw(f: &mut Frame, name: &str, cues: &[Cue], player: &Player, pos: u64) {
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
    for cue in cues.iter().take_while(|c| c.start_ms <= pos) {
        let current = pos < cue.end_ms;
        let spans: Vec<Span> = cue
            .words
            .iter()
            .map(|w| {
                let style = if !current || w.end_ms <= pos {
                    Style::new()
                } else if w.start_ms <= pos {
                    Style::new().fg(SPOKEN).add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(Color::DarkGray)
                };
                Span::styled(w.text.clone(), style)
            })
            .collect();
        let gutter = Span::styled(
            format!("[{}] ", timestamp(cue.start_ms)),
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
    f.render_widget(
        Line::from(" space pause · ←/→ 5 s · home restart · q quit").dark_gray(),
        keys,
    );
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
    fn spreads_untimed_lines() {
        let ass = "Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,a b\n";
        let cues = parse_ass(ass);
        assert_eq!(cues[0].words[1].start_ms, 1000);
    }
}
