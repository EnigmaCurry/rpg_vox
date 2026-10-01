//! Full-screen view: live transcript, level meter, key bindings.
//!
//! Colour carries the pass: dim ALL CAPS is the pass-1 streaming
//! partial, normal text is the pass-2 re-decode, and words briefly turn
//! green when pass 3 rewrites them at a boundary.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use unicode_width::UnicodeWidthStr;
use vox_transcribe::{Change, Event, Stage, Transcript};

use crate::markdown::{timestamp, HistoryBlock};
use crate::Session;

const FLASH: Duration = Duration::from_millis(1500);
const STATUS_TTL: Duration = Duration::from_secs(3);
const GUTTER: usize = 11; // "[hh:mm:ss] "

struct App {
    transcript: Transcript,
    rms: f32,
    speaking: bool,
    position_ms: u64,
    /// Clip id → when pass 3 last changed its text.
    flashes: HashMap<String, Instant>,
    /// Lines scrolled up from the bottom; 0 follows new text.
    scroll_back: u16,
    /// Paragraph highlighted for copying.
    selected: Option<String>,
    /// Footer message and when it was set; cleared after `STATUS_TTL`.
    status: Option<(String, Instant)>,
}

pub fn run(session: Session) -> Result<()> {
    let mut terminal = ratatui::init();
    let res = run_loop(&mut terminal, &session);
    // Show progress while queued passes drain.
    let _ = terminal.draw(|f| {
        let msg = Paragraph::new("Finishing queued passes and saving…").block(Block::bordered());
        f.render_widget(msg, f.area());
    });
    let finished = session.finish();
    ratatui::restore();
    res?;
    finished.map(|_| ())
}

fn run_loop(terminal: &mut DefaultTerminal, s: &Session) -> Result<()> {
    let mut app = App {
        transcript: Transcript::default(),
        rms: 0.0,
        speaking: false,
        position_ms: 0,
        flashes: HashMap::new(),
        scroll_back: 0,
        selected: None,
        status: None,
    };
    let events = s.engine.events().clone();
    loop {
        for ev in events.try_iter() {
            app.apply(ev, s);
        }
        app.flashes.retain(|_, t| t.elapsed() < FLASH);
        if app
            .status
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() >= STATUS_TTL)
        {
            app.status = None;
        }
        terminal.draw(|f| draw(f, &app, s))?;

        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        let TermEvent::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Char('c') if ctrl => return Ok(()),
            KeyCode::Char(' ') => {
                let was = s.paused.fetch_xor(true, Ordering::SeqCst);
                if !was {
                    s.engine.break_paragraph();
                }
            }
            KeyCode::Enter => s.engine.break_paragraph(),
            KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
            KeyCode::Char('y') | KeyCode::Char('c') => app.copy_selected(),
            KeyCode::PageUp => {
                app.selected = None;
                app.scroll_back = app.scroll_back.saturating_add(10);
            }
            KeyCode::PageDown => {
                app.selected = None;
                app.scroll_back = app.scroll_back.saturating_sub(10);
            }
            KeyCode::End | KeyCode::Char('G') => {
                app.selected = None;
                app.scroll_back = 0;
            }
            _ => {}
        }
    }
}

impl App {
    fn apply(&mut self, ev: Event, s: &Session) {
        match ev {
            Event::Paragraph { paragraph, change } => {
                if change == Change::Revised {
                    let old = self
                        .transcript
                        .paragraphs
                        .iter()
                        .find(|p| p.id == paragraph.id);
                    for c in &paragraph.clips {
                        let before = old.and_then(|o| o.clips.iter().find(|x| x.id == c.id));
                        if before.map(|b| b.text != c.text).unwrap_or(false) {
                            self.flashes.insert(c.id.clone(), Instant::now());
                        }
                    }
                }
                if change == Change::Hardened {
                    if let Err(e) = s.writer.lock().expect("writer lock").append(&paragraph) {
                        self.set_status(format!("write failed: {e:#}"));
                    }
                }
                self.transcript.upsert(&paragraph);
            }
            Event::ParagraphRemoved { id } => {
                if self.selected.as_deref() == Some(id.as_str()) {
                    self.selected = None;
                }
                self.transcript.remove(&id);
            }
            Event::Level {
                rms,
                speaking,
                position_ms,
            } => {
                self.rms = rms;
                self.speaking = speaking;
                self.position_ms = position_ms;
            }
        }
    }

    fn set_status(&mut self, msg: String) {
        self.status = Some((msg, Instant::now()));
    }

    /// Non-empty paragraphs, in display order.
    fn visible_ids(&self) -> Vec<&str> {
        self.transcript
            .paragraphs
            .iter()
            .filter(|p| !p.text.trim().is_empty())
            .map(|p| p.id.as_str())
            .collect()
    }

    /// Move the highlight by `delta` paragraphs; the first ↑ selects the
    /// newest paragraph.
    fn move_selection(&mut self, delta: isize) {
        let ids = self.visible_ids();
        if ids.is_empty() {
            return;
        }
        let pos = match self
            .selected
            .as_deref()
            .and_then(|s| ids.iter().position(|i| *i == s))
        {
            Some(i) => (i as isize + delta).clamp(0, ids.len() as isize - 1) as usize,
            None => ids.len() - 1,
        };
        self.selected = Some(ids[pos].to_string());
        self.scroll_back = 0;
    }

    /// Copy the highlighted paragraph, or the newest one if none is.
    fn copy_selected(&mut self) {
        let ids = self.visible_ids();
        let Some(id) = self.selected.as_deref().or(ids.last().copied()) else {
            return;
        };
        let Some(p) = self.transcript.paragraphs.iter().find(|p| p.id == id) else {
            return;
        };
        let words = p.text.split_whitespace().count();
        let msg = match crate::clipboard::copy(p.text.trim()) {
            Ok(how) => format!("copied {words} words ({how})"),
            Err(e) => format!("copy failed: {e}"),
        };
        self.set_status(msg);
    }
}

fn draw(f: &mut Frame, app: &App, s: &Session) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .areas(f.area());

    let paused = s.paused.load(Ordering::Relaxed);
    let state = if paused {
        Span::styled(" PAUSED ", Style::new().black().on_yellow())
    } else if s.source_done.load(Ordering::Relaxed) {
        Span::styled(" INPUT DONE ", Style::new().black().on_blue())
    } else {
        Span::styled(" ● REC ", Style::new().white().on_red())
    };
    let path = s
        .writer
        .lock()
        .expect("writer lock")
        .path()
        .display()
        .to_string();
    f.render_widget(
        Line::from(vec![
            state,
            format!(" {}  ", timestamp(app.position_ms)).bold(),
            Span::raw(s.device.clone()),
            format!("  → {path}").dark_gray(),
        ]),
        header,
    );

    let inner_w = body.width.saturating_sub(2) as usize;
    let (lines, ranges) = transcript_lines(app, &s.history, inner_w.max(GUTTER + 10));
    let visible = body.height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(visible);
    let mut top = max_scroll.saturating_sub(app.scroll_back as usize);
    // Keep the selected paragraph on screen.
    if let Some((_, start, end)) = ranges
        .iter()
        .find(|(id, _, _)| Some(id) == app.selected.as_ref())
    {
        if *start < top {
            top = *start;
        } else if *end > top + visible {
            top = end.saturating_sub(visible).min(*start);
        }
    }
    let title = if app.selected.is_some() {
        " Transcript (y copy, End to follow) "
    } else if app.scroll_back > 0 {
        " Transcript (scrolled, End to follow) "
    } else {
        " Transcript "
    };
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .block(Block::bordered().title(title))
            .scroll((top as u16, 0)),
        body,
    );

    let [meter_line, help_line] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(footer);
    let db = if app.rms > 0.0 {
        20.0 * app.rms.log10()
    } else {
        -90.0
    };
    let frac = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
    let width = 24usize;
    let filled = (frac * width as f32).round() as usize;
    let overruns = s
        .capture
        .as_ref()
        .map(|c| c.overruns.load(Ordering::Relaxed))
        .unwrap_or(0);
    let mut meter = vec![
        Span::raw(" level "),
        Span::styled(
            "█".repeat(filled),
            Style::new().fg(if app.speaking {
                Color::Green
            } else {
                Color::Gray
            }),
        ),
        Span::styled("░".repeat(width - filled), Style::new().dark_gray()),
        format!(" {db:>5.0} dB  ").into(),
        if app.speaking {
            "● speech".green()
        } else {
            "○ quiet".dark_gray()
        },
    ];
    if overruns > 0 {
        meter.push(format!("  dropped {overruns} samples").red());
    }
    if let Some((msg, _)) = &app.status {
        meter.push(format!("  {msg}").yellow());
    }
    f.render_widget(Line::from(meter), meter_line);
    f.render_widget(
        Line::from(vec![
            " LIVE CAPS".dark_gray().italic(),
            " · re-decoded".into(),
            " · boundary-fixed".green(),
            "   ↑↓ select · y copy · space pause · enter new paragraph · q quit".dark_gray(),
        ]),
        help_line,
    );
}

/// Word-wrap `words` behind a gutter: `label` on the first line, blank
/// (in `cont_style`) after.
fn wrap(
    label: String,
    label_style: Style,
    cont_style: Style,
    words: Vec<(String, Style)>,
    text_w: usize,
    out: &mut Vec<Line<'static>>,
) {
    let mut first = true;
    let mut line: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    let mut flush = |line: &mut Vec<Span<'static>>, out: &mut Vec<Line<'static>>| {
        let gutter = if first {
            Span::styled(format!("{label:<width$}", width = GUTTER - 1), label_style)
        } else {
            Span::styled(" ".repeat(GUTTER - 1), cont_style)
        };
        let mut spans = vec![gutter, Span::raw(" ")];
        spans.append(line);
        out.push(Line::from(spans));
        first = false;
    };
    for (w, style) in words {
        let ww = w.width();
        if used > 0 && used + 1 + ww > text_w {
            flush(&mut line, out);
            used = 0;
        }
        if used > 0 {
            line.push(Span::raw(" "));
            used += 1;
        }
        used += ww;
        line.push(Span::styled(w, style));
    }
    if !line.is_empty() {
        flush(&mut line, out);
    }
}

/// The previous session's tail (when appending), all in grey.
fn history_lines(history: &[HistoryBlock], width: usize, out: &mut Vec<Line<'static>>) {
    if history.is_empty() {
        return;
    }
    let text_w = width.saturating_sub(GUTTER).max(10);
    let grey = Style::new().dark_gray();
    for block in history {
        match block {
            HistoryBlock::Paragraph { timestamp, text } => {
                let words = text
                    .split_whitespace()
                    .map(|w| (w.to_string(), grey))
                    .collect();
                wrap(format!("[{timestamp}]"), grey, grey, words, text_w, out);
            }
            HistoryBlock::Rule => out.push(Line::styled("─".repeat(width.min(40)), grey)),
            HistoryBlock::Note(note) => out.push(Line::styled(note.clone(), grey.italic())),
        }
        out.push(Line::default());
    }
    out.push(Line::styled(
        format!("{:─^w$}", " this session ", w = width.min(40)),
        grey,
    ));
    out.push(Line::default());
}

/// Word-wrap every paragraph with a timestamp gutter, styling each word
/// by the pass that produced it. Also returns each paragraph's
/// `[start, end)` line range for keeping the selection in view.
fn transcript_lines(
    app: &App,
    history: &[HistoryBlock],
    width: usize,
) -> (Vec<Line<'static>>, Vec<(String, usize, usize)>) {
    let text_w = width.saturating_sub(GUTTER).max(10);
    let mut out = Vec::new();
    let mut ranges = Vec::new();
    history_lines(history, width, &mut out);
    for p in &app.transcript.paragraphs {
        let mut words: Vec<(String, Style)> = Vec::new();
        for c in &p.clips {
            let style = match c.stage {
                Stage::Partial => Style::new().dark_gray().add_modifier(Modifier::ITALIC),
                _ if app.flashes.contains_key(&c.id) => Style::new().green(),
                _ => Style::new(),
            };
            words.extend(c.text.split_whitespace().map(|w| (w.to_string(), style)));
        }
        if words.is_empty() {
            continue;
        }
        let selected = app.selected.as_deref() == Some(p.id.as_str());
        let gutter_style = match (selected, p.hardened) {
            (true, _) => Style::new().black().on_cyan(),
            (false, true) => Style::new().cyan(),
            (false, false) => Style::new().cyan().bold(),
        };
        let cont_style = if selected { gutter_style } else { Style::new() };
        let start_line = out.len();
        let label = format!("[{}]", timestamp(p.start_ms));
        wrap(label, gutter_style, cont_style, words, text_w, &mut out);
        ranges.push((p.id.clone(), start_line, out.len()));
        out.push(Line::default());
    }
    (out, ranges)
}
