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

use crate::markdown::timestamp;
use crate::Session;

const FLASH: Duration = Duration::from_millis(1500);
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
    status: String,
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
        status: String::new(),
    };
    let events = s.engine.events().clone();
    loop {
        let mut harden = false;
        for ev in events.try_iter() {
            match ev {
                Event::Paragraph { paragraph, change } => {
                    if change == Change::Revised {
                        let old = app
                            .transcript
                            .paragraphs
                            .iter()
                            .find(|p| p.id == paragraph.id);
                        for c in &paragraph.clips {
                            let before = old.and_then(|o| o.clips.iter().find(|x| x.id == c.id));
                            if before.map(|b| b.text != c.text).unwrap_or(false) {
                                app.flashes.insert(c.id.clone(), Instant::now());
                            }
                        }
                    }
                    harden |= change == Change::Hardened;
                    app.transcript.upsert(&paragraph);
                }
                Event::ParagraphRemoved { id } => app.transcript.remove(&id),
                Event::Level {
                    rms,
                    speaking,
                    position_ms,
                } => {
                    app.rms = rms;
                    app.speaking = speaking;
                    app.position_ms = position_ms;
                }
            }
        }
        if harden {
            match s.writer.save(&app.transcript, false) {
                Ok(()) => app.status.clear(),
                Err(e) => app.status = format!("save failed: {e:#}"),
            }
        }
        app.flashes.retain(|_, t| t.elapsed() < FLASH);
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
            KeyCode::Char('s') => {
                app.status = match s.writer.save(&app.transcript, true) {
                    Ok(()) => format!("saved {}", s.writer.path().display()),
                    Err(e) => format!("save failed: {e:#}"),
                }
            }
            KeyCode::Up | KeyCode::Char('k') => app.scroll_back = app.scroll_back.saturating_add(1),
            KeyCode::Down | KeyCode::Char('j') => {
                app.scroll_back = app.scroll_back.saturating_sub(1)
            }
            KeyCode::PageUp => app.scroll_back = app.scroll_back.saturating_add(10),
            KeyCode::PageDown => app.scroll_back = app.scroll_back.saturating_sub(10),
            KeyCode::End | KeyCode::Char('G') => app.scroll_back = 0,
            _ => {}
        }
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
    f.render_widget(
        Line::from(vec![
            state,
            format!(" {}  ", timestamp(app.position_ms)).bold(),
            Span::raw(s.device.clone()),
            format!("  → {}", s.writer.path().display()).dark_gray(),
        ]),
        header,
    );

    let inner_w = body.width.saturating_sub(2) as usize;
    let lines = transcript_lines(app, inner_w.max(GUTTER + 10));
    let visible = body.height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(visible);
    let top = max_scroll.saturating_sub(app.scroll_back as usize);
    let title = if app.scroll_back > 0 {
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
    if !app.status.is_empty() {
        meter.push(format!("  {}", app.status).yellow());
    }
    f.render_widget(Line::from(meter), meter_line);
    f.render_widget(
        Line::from(vec![
            " LIVE CAPS".dark_gray().italic(),
            " · re-decoded".into(),
            " · boundary-fixed".green(),
            "   space pause · enter new paragraph · s save · ↑↓ scroll · q quit".dark_gray(),
        ]),
        help_line,
    );
}

/// Word-wrap every paragraph with a timestamp gutter, styling each word
/// by the pass that produced it.
fn transcript_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let text_w = width.saturating_sub(GUTTER).max(10);
    let mut out = Vec::new();
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
        let gutter_style = if p.hardened {
            Style::new().cyan()
        } else {
            Style::new().cyan().bold()
        };
        let mut first = true;
        let mut line: Vec<Span<'static>> = Vec::new();
        let mut used = 0usize;
        let flush =
            |line: &mut Vec<Span<'static>>, first: &mut bool, out: &mut Vec<Line<'static>>| {
                let gutter = if *first {
                    Span::styled(format!("[{}] ", timestamp(p.start_ms)), gutter_style)
                } else {
                    Span::raw(" ".repeat(GUTTER))
                };
                let mut spans = vec![gutter];
                spans.append(line);
                out.push(Line::from(spans));
                *first = false;
            };
        for (w, style) in words {
            let ww = w.width();
            if used > 0 && used + 1 + ww > text_w {
                flush(&mut line, &mut first, &mut out);
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
            flush(&mut line, &mut first, &mut out);
        }
        out.push(Line::default());
    }
    out
}
