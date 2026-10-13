//! `--dictate NAME`: write NAME.md by voice and edit it in place, like
//! Scribe for Android's dictations. A cursor sends new speech anywhere
//! in the text; words can be selected and deleted, paragraphs broken,
//! the last sentence taken back, and the text typed by hand. The file is
//! read when it exists and saved as you go, so you can come back to it.
//!
//! Recording is start and stop, no pause. Under the hood stopping is the
//! transcription TUI's pause: the mic is released and the session clock
//! keeps running, so the editing model's times stay in order.

use std::cell::Cell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event as TermEvent, KeyCode, KeyEventKind,
    KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;
use vox_transcribe::{Event, Transcript};

use crate::dictation::{self, Doc, Tok};
use crate::tui::{normalize_key, Screen};
use crate::Session;

const STATUS_TTL: Duration = Duration::from_secs(3);
/// Saved this long after the last change (and on leaving).
const SAVE_AFTER: Duration = Duration::from_secs(2);
/// After stopping, how long a last utterance may take to show up as
/// pass-1 text before it's taken as done.
const SETTLE: Duration = Duration::from_millis(700);
const CARET: &str = "▌";

/// A gap between placed words, held by the word after it so new words
/// don't shift it.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Gap {
    Before((i64, bool)),
    End,
}

/// The text laid out at one width.
#[derive(Default)]
struct View {
    lines: Vec<Line<'static>>,
    /// The doc indices of the placed words and breaks, in order.
    placed: Vec<usize>,
    /// Where each gap between them stands, `placed.len() + 1` of them.
    gaps: Vec<(usize, usize)>,
    /// Each placed word's line and columns (breaks: none).
    spans: Vec<Option<(usize, usize, usize)>>,
    /// Gaps before the cursor.
    caret_gap: usize,
    caret_line: usize,
}

/// Tab: the text as a plain buffer for typing by hand.
struct Typing {
    text: Vec<char>,
    cursor: usize,
    /// Column kept across ↑ / ↓.
    want_col: Option<usize>,
}

struct App<'a> {
    s: &'a Session,
    path: PathBuf,
    doc: Doc,
    built: Vec<Tok>,
    transcript: Transcript,
    /// Clips already in the text (typed over), left out from then on.
    folded: HashSet<String>,
    listening: bool,
    stopped_at: Instant,
    /// Selection: where it began and where it reaches.
    sel: Option<(Gap, Gap)>,
    /// Mouse button held: the gap it went down at.
    drag: Option<Gap>,
    typing: Option<Typing>,
    /// Tab while still listening or finishing: type once the words are in.
    want_typing: bool,
    /// Keys pressed meanwhile, for the typing once it opens.
    typed_ahead: Vec<(KeyCode, bool)>,
    listen_after_typing: bool,
    /// First line shown; follows the cursor unless scrolled away.
    top: Cell<usize>,
    follow: Cell<bool>,
    /// The text box on screen, for the mouse and ↑ / ↓.
    body: Cell<Rect>,
    rms: f32,
    status: Option<(String, Instant)>,
    changed_at: Option<Instant>,
    saved: String,
}

pub fn run(mut screen: Screen, session: Session, path: PathBuf) -> Result<()> {
    let saved = match std::fs::read_to_string(&path) {
        Ok(t) => dictation::unwrap_file(&t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let mut doc = Doc::default();
    doc.load(&saved, usize::MAX, 0);
    let mut app = App {
        s: &session,
        path,
        doc,
        built: Vec::new(),
        transcript: Transcript::default(),
        folded: HashSet::new(),
        listening: !session.paused.load(Ordering::SeqCst),
        stopped_at: Instant::now(),
        sel: None,
        drag: None,
        typing: None,
        want_typing: false,
        typed_ahead: Vec::new(),
        listen_after_typing: false,
        top: Cell::new(0),
        follow: Cell::new(true),
        body: Cell::new(Rect::default()),
        rms: 0.0,
        status: None,
        changed_at: None,
        saved: saved.clone(),
    };
    app.relayout();
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let res = app.run_loop(&mut screen);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    let _ = screen.terminal().draw(|f| {
        f.render_widget(
            Paragraph::new("Finishing the last words and saving…").block(Block::bordered()),
            f.area(),
        )
    });
    if let Some(t) = app.typing.take() {
        app.load_typed(&t);
    }
    let (path, folded, saved) = (
        app.path.clone(),
        std::mem::take(&mut app.folded),
        app.saved.clone(),
    );
    let mut doc = std::mem::take(&mut app.doc);
    drop(app);
    let final_t = session.finish();
    screen.restore();
    res?;
    let final_t = final_t?;
    doc.clips = dictation::clips(&final_t.paragraphs, &folded, |_| Vec::new());
    let text = doc.text();
    if (path.exists() || !text.is_empty()) && text != saved {
        write(&path, &text)?;
    }
    if path.exists() {
        crate::say!("saved {}", path.display());
    }
    Ok(())
}

/// Write `text` to `path` whole: to a temporary file beside it, then
/// renamed over it, so a reader never sees half of it.
fn write(path: &Path, text: &str) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let body = if text.is_empty() {
        String::new()
    } else {
        format!("{text}\n")
    };
    std::fs::write(&tmp, body).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

impl App<'_> {
    fn run_loop(&mut self, screen: &mut Screen) -> Result<()> {
        let events = self.s.engine.events().clone();
        loop {
            let mut changed = false;
            for ev in events.try_iter() {
                changed |= self.apply(ev);
            }
            if changed {
                self.relayout();
                self.changed();
            }
            // The mic failed to reopen (see `LivePump`): stopped.
            if self.listening && self.s.paused.load(Ordering::SeqCst) {
                self.listening = false;
                self.stopped_at = Instant::now();
                self.set_status("the microphone couldn't be opened".into());
            }
            if self.want_typing && !self.finishing() {
                self.want_typing = false;
                self.open_typing();
                for (code, ctrl) in std::mem::take(&mut self.typed_ahead) {
                    self.typing_key(code, ctrl);
                }
            }
            if self.changed_at.is_some_and(|t| t.elapsed() >= SAVE_AFTER) {
                self.save();
            }
            if self
                .status
                .as_ref()
                .is_some_and(|(_, at)| at.elapsed() >= STATUS_TTL)
            {
                self.status = None;
            }
            screen.terminal().draw(|f| self.draw(f))?;
            if !event::poll(Duration::from_millis(50))? {
                continue;
            }
            match event::read()? {
                TermEvent::Key(key) => {
                    let key = normalize_key(key);
                    tracing::debug!(?key, "key");
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                    // Shift selects with the arrows; a capital G is just G.
                    let shift = key.modifiers.contains(KeyModifiers::SHIFT)
                        && !matches!(key.code, KeyCode::Char(_));
                    if ctrl && key.code == KeyCode::Char('c') {
                        return Ok(());
                    }
                    if self.want_typing && key.code == KeyCode::Esc {
                        // Changed my mind: back to dictating.
                        self.want_typing = false;
                        self.typed_ahead.clear();
                        if std::mem::take(&mut self.listen_after_typing) {
                            self.start();
                        }
                    } else if self.want_typing {
                        self.typed_ahead.push((key.code, ctrl));
                    } else if self.typing.is_some() {
                        self.typing_key(key.code, ctrl);
                    } else if !self.key(key.code, ctrl, shift) {
                        return Ok(());
                    }
                }
                TermEvent::Mouse(m) if self.typing.is_none() && !self.want_typing => {
                    self.mouse(m.kind, m.column, m.row)
                }
                _ => {}
            }
        }
    }

    /// An engine event; true if the text changed.
    fn apply(&mut self, ev: Event) -> bool {
        match ev {
            Event::Paragraph { paragraph, .. } => {
                self.transcript.upsert(&paragraph);
                true
            }
            Event::ParagraphRemoved { id } => {
                self.transcript.remove(&id);
                true
            }
            Event::Level { rms, .. } => {
                self.rms = rms;
                false
            }
        }
    }

    /// Stopped, but the last words may still be on their way.
    fn finishing(&self) -> bool {
        !self.listening
            && (self.stopped_at.elapsed() < SETTLE
                || self.transcript.paragraphs.iter().any(|p| p.has_partial()))
    }

    /// A key while dictating; false to quit.
    fn key(&mut self, code: KeyCode, ctrl: bool, shift: bool) -> bool {
        let view = self.view();
        match code {
            KeyCode::Char('q') => return false,
            KeyCode::Esc if self.sel.is_some() => self.sel = None,
            KeyCode::Esc => return false,
            KeyCode::Char(' ') => self.toggle(),
            KeyCode::Enter => {
                self.sel = None;
                let s = self.s;
                if self.doc.paragraph_break(&self.built, || now(s)) {
                    self.edited();
                }
            }
            KeyCode::Backspace | KeyCode::Delete => self.delete(code == KeyCode::Backspace),
            KeyCode::Tab | KeyCode::Char('i') => self.type_mode(),
            KeyCode::Char('y') | KeyCode::Char('c') => self.copy(),
            KeyCode::PageUp | KeyCode::PageDown => {
                let page = self.body.get().height.saturating_sub(3).max(1) as usize;
                let top = self.top.get();
                self.top.set(if code == KeyCode::PageUp {
                    top.saturating_sub(page)
                } else {
                    top + page
                });
                self.follow.set(false);
            }
            KeyCode::Left
            | KeyCode::Right
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::Char('g')
            | KeyCode::Char('G') => {
                let from = match self.sel {
                    Some((_, head)) if shift => self.resolve(&view, head),
                    _ => view.caret_gap,
                };
                let n = view.placed.len();
                let to = match code {
                    KeyCode::Left => from.saturating_sub(1),
                    KeyCode::Right => (from + 1).min(n),
                    KeyCode::Up => line_step(&view, from, -1),
                    KeyCode::Down => line_step(&view, from, 1),
                    KeyCode::Home | KeyCode::Char('g') if ctrl || code == KeyCode::Char('g') => 0,
                    KeyCode::End | KeyCode::Char('G') if ctrl || code == KeyCode::Char('G') => n,
                    KeyCode::Home => line_end(&view, from, false),
                    _ => line_end(&view, from, true),
                };
                if shift {
                    let anchor = match self.sel {
                        Some((a, _)) => a,
                        None => self.gap_key(&view, view.caret_gap),
                    };
                    self.sel = Some((anchor, self.gap_key(&view, to)));
                } else {
                    self.sel = None;
                    self.move_to(&view, to);
                }
                self.follow.set(true);
            }
            _ => {}
        }
        true
    }

    fn mouse(&mut self, kind: MouseEventKind, col: u16, row: u16) {
        let body = self.body.get();
        match kind {
            MouseEventKind::ScrollUp => {
                self.top.set(self.top.get().saturating_sub(3));
                self.follow.set(false);
                return;
            }
            MouseEventKind::ScrollDown => {
                self.top.set(self.top.get() + 3);
                self.follow.set(false);
                return;
            }
            _ => {}
        }
        let inside = col > body.x
            && col < body.x + body.width.saturating_sub(1)
            && row > body.y
            && row < body.y + body.height.saturating_sub(1);
        let view = self.view();
        let line = self.top.get() + (row.saturating_sub(body.y + 1)) as usize;
        let x = col.saturating_sub(body.x + 1) as usize;
        match kind {
            MouseEventKind::Down(MouseButton::Left) if inside => {
                let g = hit(&view, line, x);
                self.drag = Some(self.gap_key(&view, g));
                self.sel = None;
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(anchor) = self.drag {
                    let g = hit(&view, line, x);
                    let head = self.gap_key(&view, g);
                    self.sel = (head != anchor).then_some((anchor, head));
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(anchor) = self.drag.take() {
                    if self.sel.is_none() {
                        let g = self.resolve(&view, anchor);
                        self.move_to(&view, g);
                        self.follow.set(true);
                    }
                }
            }
            _ => {}
        }
    }

    fn toggle(&mut self) {
        if self.listening {
            self.stop();
        } else if !self.finishing() {
            self.start();
        }
    }

    fn start(&mut self) {
        self.s.paused.store(false, Ordering::SeqCst);
        self.listening = true;
        self.follow.set(true);
    }

    fn stop(&mut self) {
        if !self.listening {
            return;
        }
        self.s.paused.store(true, Ordering::SeqCst);
        self.s.engine.break_paragraph();
        self.listening = false;
        self.stopped_at = Instant::now();
        self.rms = 0.0;
    }

    /// Send new speech to gap `to`.
    fn move_to(&mut self, view: &View, to: usize) {
        let n = view.placed.len();
        if n == 0 {
            return;
        }
        let (index, after) = if to < n {
            (view.placed[to], false)
        } else {
            (view.placed[n - 1], true)
        };
        let s = self.s;
        if self.doc.move_cursor(&self.built, index, after, || now(s)) {
            self.relayout();
        }
    }

    /// Backspace: the selection, else the sentence before the cursor (or
    /// the break right before it). Delete: only the selection.
    fn delete(&mut self, sentence: bool) {
        let view = self.view();
        if let Some((a, b)) = self.sel.take() {
            let (a, b) = (self.resolve(&view, a), self.resolve(&view, b));
            let picked: Vec<&Tok> = view.placed[a.min(b)..a.max(b)]
                .iter()
                .map(|&i| &self.built[i])
                .collect();
            if !picked.is_empty() {
                let picked: Vec<Tok> = picked.into_iter().cloned().collect();
                self.doc.delete(&picked.iter().collect::<Vec<_>>());
                self.edited();
            }
            return;
        }
        if sentence && self.doc.delete_sentence(&self.built) {
            self.edited();
        }
    }

    /// `y`: the selection if there is one, else the whole text.
    fn copy(&mut self) {
        let text = match self.sel {
            Some((a, b)) => {
                let view = self.view();
                let (a, b) = (self.resolve(&view, a), self.resolve(&view, b));
                let mut text = String::new();
                for &i in &view.placed[a.min(b)..a.max(b)] {
                    let t = &self.built[i];
                    if t.brk {
                        text.push_str("\n\n");
                    } else {
                        if !text.is_empty() && !text.ends_with('\n') {
                            text.push(' ');
                        }
                        text.push_str(&t.text);
                    }
                }
                text.trim().to_string()
            }
            None => self.doc.text(),
        };
        if text.is_empty() {
            self.set_status("nothing to copy".into());
            return;
        }
        let words = text.split_whitespace().count();
        let msg = match crate::clipboard::copy(&text) {
            Ok(how) => format!("copied {words} words ({how})"),
            Err(e) => format!("copy failed: {e}"),
        };
        self.set_status(msg);
    }

    /// Tab: stop listening, and once the last words are in, type.
    fn type_mode(&mut self) {
        self.sel = None;
        if self.listening {
            self.listen_after_typing = true;
            self.stop();
        }
        self.want_typing = true;
    }

    fn open_typing(&mut self) {
        let (text, at) = self.doc.text_and_caret(&self.built);
        // Typing takes over these words; later events about them are old news.
        self.folded.extend(
            self.transcript
                .paragraphs
                .iter()
                .flat_map(|p| &p.clips)
                .map(|c| c.id.clone()),
        );
        self.typing = Some(Typing {
            text: text.chars().collect(),
            cursor: at,
            want_col: None,
        });
    }

    /// What was typed is the text again, the cursor where the typing one was.
    fn load_typed(&mut self, t: &Typing) {
        let text: String = t.text.iter().collect();
        let cursor = dictation::cursor_after_typing(&text, t.cursor);
        let origin = now(self.s);
        self.doc.load(&text, cursor, origin);
        self.relayout();
    }

    /// Esc / tab while typing: back to dictating, listening again if it was.
    fn close_typing(&mut self) {
        let Some(t) = self.typing.take() else { return };
        self.load_typed(&t);
        self.save();
        if std::mem::take(&mut self.listen_after_typing) {
            self.start();
        }
        self.follow.set(true);
    }

    fn typing_key(&mut self, code: KeyCode, ctrl: bool) {
        let width = self.body.get().width.saturating_sub(2).max(10) as usize;
        let Some(t) = self.typing.as_mut() else {
            return;
        };
        let mut keep_col = false;
        match code {
            KeyCode::Esc | KeyCode::Tab => {
                self.close_typing();
                return;
            }
            KeyCode::Char('a') if ctrl => t.cursor = line_bounds(&t.text, t.cursor).0,
            KeyCode::Char('e') if ctrl => t.cursor = line_bounds(&t.text, t.cursor).1,
            KeyCode::Char(_) if ctrl => {}
            KeyCode::Char(c) => {
                t.text.insert(t.cursor, c);
                t.cursor += 1;
            }
            KeyCode::Enter => {
                t.text.insert(t.cursor, '\n');
                t.cursor += 1;
            }
            KeyCode::Backspace if t.cursor > 0 => {
                t.cursor -= 1;
                t.text.remove(t.cursor);
            }
            KeyCode::Delete if t.cursor < t.text.len() => {
                t.text.remove(t.cursor);
            }
            KeyCode::Left => t.cursor = t.cursor.saturating_sub(1),
            KeyCode::Right => t.cursor = (t.cursor + 1).min(t.text.len()),
            KeyCode::Up | KeyCode::Down => {
                let rows = wrap_chars(&t.text, width);
                let (row, col) = locate(&rows, t.cursor);
                let col = *t.want_col.get_or_insert(col);
                let target = if code == KeyCode::Up {
                    row.checked_sub(1)
                } else {
                    (row + 1 < rows.len()).then_some(row + 1)
                };
                t.cursor = match target {
                    Some(r) => {
                        let (s, e) = rows[r];
                        // Not past a wrapped row's trailing space.
                        let last = if r + 1 < rows.len() && rows[r + 1].0 == e {
                            e.saturating_sub(1).max(s)
                        } else {
                            e
                        };
                        (s + col).min(last)
                    }
                    None if code == KeyCode::Up => 0,
                    None => t.text.len(),
                };
                keep_col = true;
            }
            KeyCode::Home => {
                let rows = wrap_chars(&t.text, width);
                t.cursor = rows[locate(&rows, t.cursor).0].0;
            }
            KeyCode::End => {
                let rows = wrap_chars(&t.text, width);
                let r = locate(&rows, t.cursor).0;
                let e = rows[r].1;
                t.cursor = if r + 1 < rows.len() && rows[r + 1].0 == e {
                    e.saturating_sub(1)
                } else {
                    e
                };
            }
            _ => {}
        }
        if !keep_col {
            t.want_col = None;
        }
        self.changed();
    }

    /// The text was changed by hand.
    fn edited(&mut self) {
        self.relayout();
        self.changed();
        self.follow.set(true);
    }

    fn changed(&mut self) {
        self.changed_at = Some(Instant::now());
    }

    fn relayout(&mut self) {
        self.doc.clips = dictation::clips(&self.transcript.paragraphs, &self.folded, |text| {
            // Live text is ALL CAPS (Zipformer).
            self.s
                .pass1
                .words(text)
                .into_iter()
                .map(|w| w.to_lowercase())
                .collect()
        });
        self.built = self.doc.build();
    }

    fn save(&mut self) {
        self.changed_at = None;
        let text = match &self.typing {
            Some(t) => t.text.iter().collect::<String>().trim().to_string(),
            None => self.doc.text(),
        };
        if text == self.saved || (text.is_empty() && !self.path.exists()) {
            return;
        }
        match write(&self.path, &text) {
            Ok(()) => self.saved = text,
            Err(e) => self.set_status(format!("{e:#}")),
        }
    }

    fn set_status(&mut self, msg: String) {
        self.status = Some((msg, Instant::now()));
    }

    /// The stable form of gap `g`.
    fn gap_key(&self, view: &View, g: usize) -> Gap {
        match view.placed.get(g) {
            Some(&i) => Gap::Before(self.built[i].key()),
            None => Gap::End,
        }
    }

    /// Gap `g` now, or the end if its word is gone.
    fn resolve(&self, view: &View, g: Gap) -> usize {
        match g {
            Gap::Before(key) => view
                .placed
                .iter()
                .position(|&i| self.built[i].key() == key)
                .unwrap_or(view.placed.len()),
            Gap::End => view.placed.len(),
        }
    }

    /// The text laid out in the text box as last drawn.
    fn view(&self) -> View {
        let width = self.body.get().width.saturating_sub(2).max(10) as usize;
        self.lay(width)
    }

    fn lay(&self, width: usize) -> View {
        let doc = &self.built;
        let (pieces, caret) = self.doc.pieces(doc);
        let selected = self.sel.map(|(a, b)| {
            let probe = View {
                placed: pieces
                    .iter()
                    .copied()
                    .filter(|&i| doc[i].placed())
                    .collect(),
                ..Default::default()
            };
            let (a, b) = (self.resolve(&probe, a), self.resolve(&probe, b));
            a.min(b)..a.max(b)
        });
        let mut v = View::default();
        let mut line: Vec<Span<'static>> = Vec::new();
        let mut col = 0usize;
        let partial = Style::new().dark_gray().add_modifier(Modifier::ITALIC);
        // Place a word-like item; returns its line and starting column.
        let put = |text: String,
                   style: Style,
                   line: &mut Vec<Span<'static>>,
                   col: &mut usize,
                   lines: &mut Vec<Line<'static>>| {
            let w = text.width();
            if *col > 0 && *col + 1 + w > width {
                lines.push(Line::from(std::mem::take(line)));
                *col = 0;
            }
            if *col > 0 {
                line.push(Span::raw(" "));
                *col += 1;
            }
            let at = (lines.len(), *col);
            line.push(Span::styled(text, style));
            *col += w;
            at
        };
        let mut caret_at = (0, 0);
        // The gap after the last placed word or break.
        let mut end = (0, 0);
        for (n, &i) in pieces.iter().enumerate() {
            if n == caret {
                caret_at = put(
                    CARET.into(),
                    Style::new().magenta(),
                    &mut line,
                    &mut col,
                    &mut v.lines,
                );
                v.caret_gap = v.placed.len();
            }
            let t = &doc[i];
            let k = v.placed.len();
            let sel = selected.as_ref().is_some_and(|r| r.contains(&k));
            if t.brk {
                v.placed.push(i);
                v.gaps
                    .push((v.lines.len(), if col > 0 { col + 1 } else { 0 }));
                v.spans.push(None);
                v.lines.push(Line::from(std::mem::take(&mut line)));
                v.lines.push(Line::default());
                col = 0;
                end = (v.lines.len(), 0);
                continue;
            }
            let style = match (t.partial, sel) {
                (true, _) => partial,
                (false, true) => Style::new().add_modifier(Modifier::REVERSED),
                (false, false) => Style::new(),
            };
            let (l, c) = put(t.text.clone(), style, &mut line, &mut col, &mut v.lines);
            if t.placed() {
                v.placed.push(i);
                v.gaps.push((l, c));
                v.spans.push(Some((l, c, c + t.text.width())));
                end = (l, c + t.text.width() + 1);
            }
        }
        if caret >= pieces.len() {
            caret_at = put(
                CARET.into(),
                Style::new().magenta(),
                &mut line,
                &mut col,
                &mut v.lines,
            );
            v.caret_gap = v.placed.len();
        }
        v.gaps.push(end);
        v.lines.push(Line::from(line));
        v.caret_line = caret_at.0;
        v
    }

    fn draw(&self, f: &mut Frame) {
        let [header, body, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .areas(f.area());
        self.body.set(body);
        let typing = self.typing.is_some() || self.want_typing && !self.listening;
        let finishing = self.finishing();
        let state = if self.listening {
            Span::styled(" ● DICTATING ", Style::new().white().on_red())
        } else if finishing {
            Span::styled(" … FINISHING ", Style::new().black().on_yellow())
        } else if typing {
            Span::styled(" ⌨ TYPING ", Style::new().black().on_cyan())
        } else {
            Span::styled(" ■ STOPPED ", Style::new().black().on_gray())
        };
        let badge = " ✍ DICTATION ";
        let [left, corner] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(badge.width() as u16)])
                .areas(header);
        f.render_widget(
            Line::from(Span::styled(badge, Style::new().black().on_magenta())),
            corner,
        );
        f.render_widget(
            Line::from(vec![
                state,
                Span::raw(format!("  {}", self.s.device())),
                format!("  → {}", self.path.display()).dark_gray(),
            ]),
            left,
        );

        let inner_w = body.width.saturating_sub(2).max(10) as usize;
        let visible = body.height.saturating_sub(2) as usize;
        if let Some(t) = &self.typing {
            self.draw_typing(f, body, t, inner_w, visible);
        } else {
            let view = self.lay(inner_w);
            let max_top = view.lines.len().saturating_sub(visible);
            let mut top = self.top.get().min(max_top);
            if self.follow.get() {
                // The cursor in view, with a line's room below it.
                if view.caret_line < top {
                    top = view.caret_line;
                } else if view.caret_line + 2 > top + visible {
                    top = (view.caret_line + 2).saturating_sub(visible).min(max_top);
                }
            }
            self.top.set(top);
            let title = if self.sel.is_some() {
                " Dictation (backspace deletes the selection, esc cancels) "
            } else if !self.follow.get() {
                " Dictation (scrolled; an arrow key returns to the cursor) "
            } else {
                " Dictation "
            };
            f.render_widget(
                Paragraph::new(Text::from(view.lines))
                    .block(Block::bordered().title(title))
                    .scroll((top as u16, 0)),
                body,
            );
        }

        let [meter_line, status_line, help_line] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(footer);
        let db = if self.rms > 0.0 && self.listening {
            20.0 * self.rms.log10()
        } else {
            -90.0
        };
        let frac = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
        let width = 24usize;
        let filled = (frac * width as f32).round() as usize;
        let mut meter = vec![
            Span::raw(" level "),
            Span::styled("█".repeat(filled), Style::new().fg(Color::Green)),
            Span::styled("░".repeat(width - filled), Style::new().dark_gray()),
        ];
        match self.s.denoise {
            Some(db) if db.is_finite() => meter.push(format!("  ◆ denoise ≤{db:.0} dB").magenta()),
            Some(_) => meter.push("  ◆ denoise".magenta()),
            None => {}
        }
        f.render_widget(Line::from(meter), meter_line);
        let hint = match &self.status {
            Some((msg, _)) => format!(" {msg}").yellow(),
            None if self.typing.is_some() => {
                " typing by hand; esc goes back to dictating at the cursor".dark_gray()
            }
            None if finishing => " finishing the last words…".dark_gray(),
            None if self.listening => " listening: new speech goes in at the cursor".dark_gray(),
            None if self.built.iter().any(|t| self.doc.shown(t)) => {
                " space to dictate at the cursor".dark_gray()
            }
            None => " space to dictate".dark_gray(),
        };
        f.render_widget(Line::from(hint), status_line);
        let keys = if self.typing.is_some() {
            " type · enter new paragraph · arrows move · esc / tab back to dictating · ctrl+c quit"
        } else if self.listening {
            " space stop · arrows/click cursor · shift/drag select · enter ¶ · ⌫ sentence · tab type · y copy · q quit"
        } else {
            " space dictate · arrows/click cursor · shift/drag select · enter ¶ · ⌫ sentence · tab type · y copy · q quit"
        };
        f.render_widget(Line::from(keys.dark_gray()), help_line);
    }

    fn draw_typing(&self, f: &mut Frame, body: Rect, t: &Typing, width: usize, visible: usize) {
        let rows = wrap_chars(&t.text, width);
        let (row, col) = locate(&rows, t.cursor);
        let mut lines: Vec<Line> = Vec::new();
        for (r, &(s, e)) in rows.iter().enumerate() {
            let text: String = t.text[s..e].iter().filter(|&&c| c != '\n').collect();
            if r != row {
                lines.push(Line::raw(text));
                continue;
            }
            let chars: Vec<char> = text.chars().collect();
            let before: String = chars[..col.min(chars.len())].iter().collect();
            let at: String = chars.get(col).map_or(" ".into(), |c| c.to_string());
            let after: String = chars.get(col + 1..).unwrap_or(&[]).iter().collect();
            lines.push(Line::from(vec![
                Span::raw(before),
                Span::styled(at, Style::new().add_modifier(Modifier::REVERSED)),
                Span::raw(after),
            ]));
        }
        let max_top = rows.len().saturating_sub(visible);
        let mut top = self.top.get().min(max_top);
        if row < top {
            top = row;
        } else if row >= top + visible {
            top = row + 1 - visible;
        }
        self.top.set(top);
        f.render_widget(
            Paragraph::new(lines)
                .block(Block::bordered().title(" Typing ").cyan())
                .scroll((top as u16, 0)),
            body,
        );
    }
}

/// The session's clock, ending the utterance in progress so that what's
/// said next starts after it.
fn now(s: &Session) -> i64 {
    s.engine.break_paragraph();
    s.slots
        .lock()
        .expect("slots lock")
        .first()
        .map_or(0, |slot| slot.fed_ms() as i64)
}

/// The gap on the line `dir` away from gap `from`'s, nearest its column.
fn line_step(view: &View, from: usize, dir: isize) -> usize {
    let (line, col) = view.gaps[from];
    let lines: Vec<usize> = view.gaps.iter().map(|g| g.0).collect();
    let target = if dir < 0 {
        lines.iter().copied().filter(|&l| l < line).max()
    } else {
        lines.iter().copied().filter(|&l| l > line).min()
    };
    let Some(target) = target else {
        return if dir < 0 { 0 } else { view.gaps.len() - 1 };
    };
    (0..view.gaps.len())
        .filter(|&g| view.gaps[g].0 == target)
        .min_by_key(|&g| view.gaps[g].1.abs_diff(col))
        .unwrap_or(from)
}

/// The first (or last) gap on gap `from`'s line.
fn line_end(view: &View, from: usize, last: bool) -> usize {
    let line = view.gaps[from].0;
    let on: Vec<usize> = (0..view.gaps.len())
        .filter(|&g| view.gaps[g].0 == line)
        .collect();
    let g = if last { on.last() } else { on.first() };
    g.copied().unwrap_or(from)
}

/// The gap nearest a click at `line`, column `x`: before or after the
/// word clicked, by which half; below the text, the end.
fn hit(view: &View, line: usize, x: usize) -> usize {
    let n = view.placed.len();
    let words: Vec<(usize, usize, usize)> = view
        .spans
        .iter()
        .enumerate()
        .filter_map(|(k, s)| s.filter(|s| s.0 == line).map(|s| (k, s.1, s.2)))
        .collect();
    if words.is_empty() {
        let last = view.spans.iter().flatten().map(|s| s.0).max();
        if last.is_none_or(|l| line > l) {
            return n;
        }
        // A blank line between paragraphs: the start of the next one.
        return view
            .spans
            .iter()
            .position(|s| s.is_some_and(|s| s.0 > line))
            .unwrap_or(n);
    }
    let (k, s, e) = *words
        .iter()
        .min_by_key(|(_, s, e)| {
            if x < *s {
                s - x
            } else if x >= *e {
                x + 1 - e
            } else {
                0
            }
        })
        .expect("a word");
    if x * 2 >= s + e {
        k + 1
    } else {
        k
    }
}

/// `text` wrapped at `width` into rows of `[start, end)` char ranges; a
/// row ends after the space it breaks at, or after its new line.
fn wrap_chars(text: &[char], width: usize) -> Vec<(usize, usize)> {
    let mut rows = Vec::new();
    let mut start = 0;
    loop {
        let end = text[start..]
            .iter()
            .position(|&c| c == '\n')
            .map_or(text.len(), |p| start + p);
        let mut i = start;
        while end - i > width {
            let brk = (i + 1..=i + width)
                .rev()
                .find(|&j| text[j - 1] == ' ')
                .unwrap_or(i + width);
            rows.push((i, brk));
            i = brk;
        }
        if end < text.len() {
            rows.push((i, end + 1));
            start = end + 1;
        } else {
            rows.push((i, end));
            break;
        }
    }
    rows
}

/// The row and column of char `at` in `rows`.
fn locate(rows: &[(usize, usize)], at: usize) -> (usize, usize) {
    for (r, &(s, e)) in rows.iter().enumerate() {
        let last = r + 1 == rows.len();
        // A position at a row's end belongs to the next row, unless it's
        // the last, or the row ends in a new line (then it's before it).
        if at >= s && (at < e || last) {
            return (r, at - s);
        }
    }
    (rows.len() - 1, at - rows[rows.len() - 1].0)
}

/// The start and end (before its new line) of the line holding `at`.
fn line_bounds(text: &[char], at: usize) -> (usize, usize) {
    let s = text[..at]
        .iter()
        .rposition(|&c| c == '\n')
        .map_or(0, |p| p + 1);
    let e = text[at..]
        .iter()
        .position(|&c| c == '\n')
        .map_or(text.len(), |p| at + p);
    (s, e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_wraps_at_spaces() {
        let text: Vec<char> = "one two three\nfour".chars().collect();
        let rows = wrap_chars(&text, 8);
        let strs: Vec<String> = rows
            .iter()
            .map(|&(s, e)| text[s..e].iter().collect())
            .collect();
        assert_eq!(strs, vec!["one two ", "three\n", "four"]);
        assert_eq!(locate(&rows, 8), (1, 0));
        assert_eq!(locate(&rows, 13), (1, 5));
        assert_eq!(locate(&rows, 14), (2, 0));
        assert_eq!(locate(&rows, 18), (2, 4));
        assert_eq!(wrap_chars(&[], 8), vec![(0, 0)]);
    }
}
