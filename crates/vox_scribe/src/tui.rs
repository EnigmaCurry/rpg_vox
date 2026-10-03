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
use vox_transcribe::correct::changed_words;
use vox_transcribe::{Change, Event, ParagraphMode, Pass4, Stage, Transcript};

use crate::markdown::{timestamp, HistoryBlock};
use crate::speakers;
use crate::{Outcome, Session};

const FLASH: Duration = Duration::from_millis(1500);
/// Bright sky blue (xterm 256-colour 117) for pass-4 text. The basic ANSI
/// blues are too dark on black in macOS Terminal.
const LLM_BLUE: Color = Color::Indexed(117);
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
    /// Show pre-pass-4 text instead of the LLM-corrected text.
    show_original: bool,
    /// Filename being typed after `s`.
    prompt: Option<String>,
}

/// The terminal in TUI mode. Restored on drop, so an error while loading
/// models still leaves the shell usable.
pub struct Screen {
    terminal: DefaultTerminal,
    /// Kitty keyboard protocol is on (pop it when restoring).
    enhanced: bool,
    restored: bool,
}

impl Screen {
    /// Enter the TUI and show a loading frame.
    pub fn splash() -> Self {
        Self::enter(" loading models…")
    }

    /// Enter the TUI showing `msg` until the first real frame.
    pub fn enter(msg: &str) -> Self {
        use ratatui::crossterm::event::{KeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
        use ratatui::crossterm::{execute, terminal::supports_keyboard_enhancement};
        let mut terminal = ratatui::init();
        // Ask for the kitty keyboard protocol so Shift+Enter is distinguishable
        // from Enter (Ghostty, kitty, WezTerm, iTerm2; not macOS Terminal).
        let enhanced = supports_keyboard_enhancement().unwrap_or(false)
            && execute!(
                std::io::stdout(),
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )
            .is_ok();
        let _ = terminal.draw(|f| {
            let msg = Paragraph::new(msg)
                .dark_gray()
                .block(Block::bordered().title(" vox_scribe "));
            f.render_widget(msg, f.area());
        });
        Self {
            terminal,
            enhanced,
            restored: false,
        }
    }

    pub fn terminal(&mut self) -> &mut DefaultTerminal {
        &mut self.terminal
    }

    pub fn restore(&mut self) {
        if !self.restored {
            if self.enhanced {
                use ratatui::crossterm::{event::PopKeyboardEnhancementFlags, execute};
                let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
            }
            ratatui::restore();
            self.restored = true;
        }
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        self.restore();
    }
}

pub fn run(mut screen: Screen, session: Session) -> Result<Outcome> {
    let terminal = &mut screen.terminal;
    let res = run_loop(terminal, &session);
    // Show progress while queued passes drain.
    let _ = terminal.draw(|f| {
        let msg = Paragraph::new("Finishing queued passes and saving…").block(Block::bordered());
        f.render_widget(msg, f.area());
    });
    let path = session.path();
    let finished = session.finish();
    screen.restore();
    let confirmed = res?;
    Ok(Outcome {
        transcript: finished?,
        path,
        confirmed,
    })
}

/// Returns true when --once ended with Enter (copy the text), false on quit.
fn run_loop(terminal: &mut DefaultTerminal, s: &Session) -> Result<bool> {
    let mut app = App {
        transcript: Transcript::default(),
        rms: 0.0,
        speaking: false,
        position_ms: 0,
        flashes: HashMap::new(),
        scroll_back: 0,
        selected: None,
        show_original: false,
        status: None,
        prompt: None,
    };
    let events = s.engine.events().clone();
    loop {
        if s.finish_requested.load(Ordering::SeqCst) {
            return Ok(true);
        }
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
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        if let Some(name) = app.prompt.as_mut() {
            match key.code {
                KeyCode::Char('c') if ctrl => return Ok(false),
                KeyCode::Esc => app.prompt = None,
                KeyCode::Enter => app.save_as(s),
                KeyCode::Backspace => {
                    name.pop();
                }
                KeyCode::Char(c) => name.push(c),
                _ => {}
            }
            continue;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(false),
            KeyCode::Char('c') if ctrl => return Ok(false),
            // In --once, Shift+Enter starts a new paragraph and Enter finishes.
            KeyCode::Enter if s.once && !shift => return Ok(true),
            KeyCode::Char(' ') => {
                let was = s.paused.fetch_xor(true, Ordering::SeqCst);
                if !was {
                    s.engine.break_paragraph();
                }
            }
            KeyCode::Enter => s.engine.break_paragraph(),
            KeyCode::Char('o') if s.llm => {
                app.show_original = !app.show_original;
                app.set_status(if app.show_original {
                    "showing text before LLM correction".into()
                } else {
                    "showing LLM-corrected text".into()
                });
            }
            KeyCode::Char('m') if !s.once => {
                let manual = !s.manual.fetch_xor(true, Ordering::SeqCst);
                let mode = if manual {
                    ParagraphMode::Manual
                } else {
                    ParagraphMode::Auto
                };
                s.engine.set_paragraph_mode(mode);
                app.set_status(if manual {
                    "manual paragraphs: press enter to break".into()
                } else {
                    "auto paragraphs: silence breaks".into()
                });
            }
            KeyCode::Char('s') if !s.once => match s.path() {
                Some(p) => app.set_status(format!("already saving to {}", p.display())),
                None => {
                    let name = chrono::Local::now()
                        .format("transcript-%Y%m%d-%H%M%S.md")
                        .to_string();
                    app.prompt = Some(name);
                }
            },
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
                    if let Err(e) = s.write(&paragraph) {
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

    /// Create the typed file in the current directory, dump the settled
    /// paragraphs into it, and keep appending from then on.
    fn save_as(&mut self, s: &Session) {
        let name = self
            .prompt
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_string();
        if name.is_empty() {
            self.prompt = None;
            return;
        }
        let path = std::path::PathBuf::from(&name);
        if path.exists() {
            self.set_status(format!("{name} exists, pick another name"));
            return;
        }
        let settled: Vec<_> = self
            .transcript
            .paragraphs
            .iter()
            .filter(|p| p.hardened)
            .collect();
        match s.save_as(path, &settled) {
            Ok(()) => {
                self.prompt = None;
                self.set_status(format!("saving to {name}"));
            }
            Err(e) => self.set_status(format!("save failed: {e:#}")),
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
        Constraint::Length(3),
    ])
    .areas(f.area());

    let paused = s.paused.load(Ordering::Relaxed);
    let state = if paused {
        Span::styled(" PAUSED ", Style::new().black().on_yellow())
    } else if s.source_done.load(Ordering::Relaxed) {
        Span::styled(" INPUT DONE ", Style::new().black().bg(LLM_BLUE))
    } else {
        Span::styled(" ● REC ", Style::new().white().on_red())
    };
    let path = s
        .path()
        .map(|p| format!("  → {}", p.display()))
        .unwrap_or_default();
    f.render_widget(
        Line::from(vec![
            state,
            if s.once {
                Span::styled(" ONE-SHOT ", Style::new().black().on_magenta())
            } else if s.manual.load(Ordering::Relaxed) {
                Span::styled(" ¶ MANUAL ", Style::new().black().on_magenta())
            } else {
                Span::styled(" ¶ AUTO ", Style::new().black().on_gray())
            },
            format!(" {}  ", timestamp(app.position_ms)).bold(),
            Span::raw(s.device.clone()),
            path.dark_gray(),
        ]),
        header,
    );

    let inner_w = body.width.saturating_sub(2) as usize;
    let (lines, ranges) = transcript_lines(
        app,
        &s.history,
        inner_w.max(GUTTER + 10),
        s.manual.load(Ordering::Relaxed),
    );
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

    let [meter_line, legend_line, help_line] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(footer);
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
        .lock()
        .expect("capture lock")
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
            if s.llm {
                " · LLM-fixed".fg(LLM_BLUE)
            } else {
                "".into()
            },
        ]),
        legend_line,
    );
    let help = match &app.prompt {
        Some(name) => Line::from(vec![
            " save as: ".yellow(),
            Span::raw(name.clone()),
            Span::styled("▌", Style::new().magenta()),
            "   enter save · esc cancel".dark_gray(),
        ]),
        None => Line::from(
            if s.once {
                " enter finish & copy · shift+enter new paragraph · space pause · q cancel"
            } else if s.llm {
                " ↑↓ select · y copy · o original · space pause · enter new paragraph · m mode · s save · q quit"
            } else {
                " ↑↓ select · y copy · space pause · enter new paragraph · m mode · s save · q quit"
            }
            .dark_gray(),
        ),
    };
    f.render_widget(help, help_line);
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
    manual: bool,
) -> (Vec<Line<'static>>, Vec<(String, usize, usize)>) {
    let text_w = width.saturating_sub(GUTTER).max(10);
    let mut out = Vec::new();
    let mut ranges = Vec::new();
    history_lines(history, width, &mut out);
    for p in &app.transcript.paragraphs {
        let mut words: Vec<(String, Style)> = Vec::new();
        if let (Some(Pass4::Done { original, .. }), false) = (&p.pass4, app.show_original) {
            // Pass 4 rewrote the paragraph text; mark the words it changed.
            let before: Vec<&str> = original.split_whitespace().collect();
            let after: Vec<&str> = p.text.split_whitespace().collect();
            let changed = changed_words(&before, &after);
            for (w, ch) in after.iter().zip(changed) {
                let style = if ch {
                    Style::new().fg(LLM_BLUE)
                } else {
                    Style::new()
                };
                words.push((w.to_string(), style));
            }
        }
        let per_clip = words.is_empty();
        for c in p.clips.iter().filter(|_| per_clip) {
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
        if let Some(s) = &p.speaker {
            let style = Style::new().fg(speakers::color(s)).bold();
            words.insert(0, (format!("{s}:"), style));
            words.insert(0, ("Speaker".to_string(), style));
        }
        let selected = app.selected.as_deref() == Some(p.id.as_str());
        let gutter_style = match (selected, p.hardened, &p.pass4) {
            (true, _, _) => Style::new().black().on_cyan(),
            (false, false, Some(Pass4::Running)) => Style::new().fg(LLM_BLUE).bold(),
            (false, _, Some(Pass4::Failed { .. })) => Style::new().red(),
            (false, true, _) => Style::new().cyan(),
            (false, false, _) => Style::new().cyan().bold(),
        };
        let cont_style = if selected { gutter_style } else { Style::new() };
        let start_line = out.len();
        let label = format!("[{}]", timestamp(p.start_ms));
        wrap(label, gutter_style, cont_style, words, text_w, &mut out);
        ranges.push((p.id.clone(), start_line, out.len()));
        out.push(Line::default());
    }
    if manual {
        place_cursor(app, width, &mut out);
    }
    (out, ranges)
}

/// Manual mode: mark where the next utterance will land, at the end of
/// the open paragraph or on a fresh line after a break.
fn place_cursor(app: &App, width: usize, out: &mut Vec<Line<'static>>) {
    let cursor = Span::styled("▌", Style::new().magenta());
    let open = app
        .transcript
        .paragraphs
        .iter()
        .rev()
        .find(|p| !p.text.trim().is_empty())
        .is_some_and(|p| !p.closed && !p.hardened);
    // The open paragraph's last line sits just before its trailing blank.
    if open && out.len() >= 2 {
        let idx = out.len() - 2;
        if out[idx].width() + 2 <= width {
            out[idx].spans.push(Span::raw(" "));
            out[idx].spans.push(cursor);
        } else {
            out.insert(
                idx + 1,
                Line::from(vec![Span::raw(" ".repeat(GUTTER)), cursor]),
            );
        }
    } else {
        out.push(Line::from(vec![Span::raw(" ".repeat(GUTTER)), cursor]));
    }
}
