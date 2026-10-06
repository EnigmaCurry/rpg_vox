//! `--chat NAME`: talk with a responder through NAME.db (see
//! [`crate::chatdb`]). What you say is held until Enter sends it. Then
//! it's the responder's turn: the mic is muted (half duplex, so it
//! doesn't hear the reply) until the reply has been read aloud and the
//! responder says it has no more. Space holds the reply and opens the
//! mic: start talking and the reply is cut off, or press space again to
//! hear it again from the start.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;
use vox_transcribe::{Change, Event, Stage, Transcript};

use crate::chatdb::{Db, Kind, Role, Row};
use crate::tui::{normalize_key, Screen};
use crate::voice::Voice;
use crate::Session;

const STATUS_TTL: Duration = Duration::from_secs(3);
/// Keep the mic muted this long after the reply stops sounding, so its
/// tail isn't heard as speech.
const TAIL: Duration = Duration::from_millis(300);
const YOU: Color = Color::Cyan;
const THEM: Color = Color::Indexed(117);
const GUTTER: usize = 11;

struct Turn {
    role: Role,
    /// The user message this answers (assistant turns).
    reply_to: Option<i64>,
    text: String,
    /// Cut off by the user.
    cut: bool,
}

struct Chat<'a> {
    s: &'a Session,
    db: Db,
    voice: Voice,
    transcript: Transcript,
    turns: Vec<Turn>,
    /// Last database row seen.
    last_row: i64,
    /// The user message whose reply is expected or playing.
    awaiting: Option<i64>,
    /// The responder said more is coming for `awaiting`.
    more: bool,
    /// Paragraphs sent or discarded: no longer the input.
    done: HashSet<String>,
    /// Paragraphs sent with Enter, waiting for their last pass.
    sending: Vec<String>,
    /// The reply word last heard: (user message it answers, word index).
    heard: Option<(i64, usize)>,
    /// Space during the responder's turn: reply paused, mic open.
    held: bool,
    /// ↑/↓: the reply highlighted in the scrollback (index into `turns`).
    selected: Option<usize>,
    /// An earlier reply being played again with enter (its `reply_to`).
    playing: Option<i64>,
    /// When the mic was last wanted open, for [`TAIL`].
    open_since: Option<Instant>,
    rms: f32,
    speaking: bool,
    scroll_back: u16,
    status: Option<(String, Instant)>,
    db_path: PathBuf,
    /// `e`: the file name being typed, and whether enter has already
    /// been pressed once on an existing file (a second press replaces it).
    export: Option<(String, bool)>,
    /// Typed or edited text (`i`) waiting to be sent, ahead of anything
    /// transcribed since.
    draft: Option<String>,
    /// The `i` editor, while open.
    editor: Option<Editor>,
}

/// `i`: a text box over the chat for typing or fixing the unsent text.
struct Editor {
    text: Vec<char>,
    /// Char index the next character goes in at.
    cursor: usize,
    /// The transcribed paragraphs it was opened with; applying the edit
    /// replaces them.
    ids: Vec<String>,
}

impl Editor {
    /// A key; `Some(true)` applies the edit, `Some(false)` drops it.
    fn key(&mut self, code: KeyCode, ctrl: bool) -> Option<bool> {
        match code {
            KeyCode::Esc => return Some(false),
            KeyCode::Char('q') if ctrl => return Some(false),
            KeyCode::Char('c') if ctrl => return Some(false),
            KeyCode::Enter => return Some(true),
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.text.len(),
            KeyCode::Char('u') if ctrl => {
                self.text.drain(..self.cursor);
                self.cursor = 0;
            }
            KeyCode::Char(_) if ctrl => {}
            KeyCode::Char(c) => {
                self.text.insert(self.cursor, c);
                self.cursor += 1;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                self.text.remove(self.cursor);
            }
            KeyCode::Delete if self.cursor < self.text.len() => {
                self.text.remove(self.cursor);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.text.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            _ => {}
        }
        None
    }

    fn draw(&self, f: &mut Frame, area: ratatui::layout::Rect) {
        use ratatui::widgets::{Clear, Wrap};
        let w = (area.width * 4 / 5).max(30).min(area.width);
        let h = 10.min(area.height);
        let rect = ratatui::layout::Rect {
            x: area.x + (area.width - w) / 2,
            y: area.y + (area.height - h) / 2,
            width: w,
            height: h,
        };
        let before: String = self.text[..self.cursor].iter().collect();
        let at: String = self
            .text
            .get(self.cursor)
            .map(|c| c.to_string())
            .unwrap_or_else(|| " ".into());
        let after: String = self
            .text
            .get(self.cursor + 1..)
            .unwrap_or(&[])
            .iter()
            .collect();
        let line = Line::from(vec![
            Span::raw(before),
            Span::styled(at, Style::new().add_modifier(Modifier::REVERSED)),
            Span::raw(after),
        ]);
        f.render_widget(Clear, rect);
        f.render_widget(
            Paragraph::new(line).wrap(Wrap { trim: false }).block(
                Block::bordered().title(" Message ").title_bottom(
                    Line::from(" enter apply (enter again sends) · esc / ctrl+q discard edit ")
                        .dark_gray(),
                ),
            ),
            rect,
        );
    }
}

pub fn run(mut screen: Screen, session: Session, db_path: &Path, voice: Voice) -> Result<()> {
    let db = Db::open(db_path)?;
    let last_row = db.last_id()?;
    let mut chat = Chat {
        s: &session,
        voice,
        transcript: Transcript::default(),
        turns: history(&db)?,
        db,
        last_row,
        awaiting: None,
        more: false,
        done: HashSet::new(),
        sending: Vec::new(),
        heard: None,
        held: false,
        selected: None,
        playing: None,
        open_since: Some(Instant::now()),
        rms: 0.0,
        speaking: false,
        scroll_back: 0,
        status: None,
        db_path: db_path.to_path_buf(),
        export: None,
        draft: None,
        editor: None,
    };
    let res = chat.run_loop(&mut screen, db_path);
    chat.voice.stop();
    session.muted.store(false, Ordering::SeqCst);
    let _ = screen.terminal().draw(|f| {
        f.render_widget(
            Paragraph::new("Stopping…").block(Block::bordered()),
            f.area(),
        )
    });
    drop(chat);
    let finished = session.finish();
    screen.restore();
    res?;
    finished?;
    Ok(())
}

/// Earlier turns in the database, for context.
fn history(db: &Db) -> Result<Vec<Turn>> {
    let mut turns: Vec<Turn> = Vec::new();
    for r in db.since(0)? {
        match (r.role, r.kind) {
            (Role::User, Kind::Say) => turns.push(Turn {
                role: Role::User,
                reply_to: None,
                text: r.text,
                cut: false,
            }),
            (Role::User, Kind::Interrupt) => {
                if let Some(t) = turns
                    .iter_mut()
                    .rev()
                    .find(|t| t.role == Role::Assistant && t.reply_to == r.reply_to)
                {
                    t.cut = true;
                }
            }
            (Role::Assistant, _) => push_reply(&mut turns, r.reply_to, &r.text),
        }
    }
    Ok(turns)
}

/// Add a reply chunk to the reply it continues, or start one.
fn push_reply(turns: &mut Vec<Turn>, reply_to: Option<i64>, text: &str) {
    let text = text.trim();
    match turns.last_mut() {
        Some(t) if t.role == Role::Assistant && t.reply_to == reply_to => {
            if !text.is_empty() {
                if !t.text.is_empty() {
                    t.text.push(' ');
                }
                t.text.push_str(text);
            }
        }
        _ => turns.push(Turn {
            role: Role::Assistant,
            reply_to,
            text: text.to_string(),
            cut: false,
        }),
    }
}

impl Chat<'_> {
    fn run_loop(&mut self, screen: &mut Screen, db_path: &Path) -> Result<()> {
        let events = self.s.engine.events().clone();
        let name = db_path.display().to_string();
        loop {
            for ev in events.try_iter() {
                self.apply(ev);
            }
            self.tick()?;
            if self
                .status
                .as_ref()
                .is_some_and(|(_, at)| at.elapsed() >= STATUS_TTL)
            {
                self.status = None;
            }
            screen.terminal().draw(|f| self.draw(f, &name))?;
            if !event::poll(Duration::from_millis(30))? {
                continue;
            }
            let TermEvent::Key(key) = event::read()? else {
                continue;
            };
            let key = normalize_key(key);
            if key.kind != KeyEventKind::Press {
                continue;
            }
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            if let Some(ed) = self.editor.as_mut() {
                match ed.key(key.code, ctrl) {
                    Some(true) => self.apply_edit(),
                    Some(false) => self.editor = None,
                    None => {}
                }
                continue;
            }
            if let Some((name, confirm)) = self.export.as_mut() {
                match key.code {
                    KeyCode::Char('c') if ctrl => return Ok(()),
                    KeyCode::Esc => self.export = None,
                    KeyCode::Enter => self.export_to(),
                    KeyCode::Backspace => {
                        name.pop();
                        *confirm = false;
                    }
                    KeyCode::Char(c) => {
                        name.push(c);
                        *confirm = false;
                    }
                    _ => {}
                }
                continue;
            }
            match key.code {
                KeyCode::Char('i') => self.open_editor(),
                KeyCode::Char('e') => {
                    let name = self.db_path.with_extension("md");
                    self.export = Some((name.display().to_string(), false));
                }
                // Out of the scrollback first; quit only from the bottom.
                KeyCode::Char('q') | KeyCode::Esc
                    if self.selected.is_some() || self.scroll_back > 0 =>
                {
                    self.selected = None;
                    self.scroll_back = 0;
                }
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('c') if ctrl => return Ok(()),
                KeyCode::Enter => match self.selected {
                    Some(i) => self.play_turn(i),
                    None => self.send(),
                },
                KeyCode::Backspace => self.discard(),
                KeyCode::Char(' ') => self.space(),
                KeyCode::Up => self.select(-1),
                KeyCode::Down => self.select(1),
                KeyCode::PageUp => {
                    self.selected = None;
                    self.scroll_back = self.scroll_back.saturating_add(10);
                }
                KeyCode::PageDown => {
                    self.selected = None;
                    self.scroll_back = self.scroll_back.saturating_sub(10);
                }
                KeyCode::End => {
                    self.selected = None;
                    self.scroll_back = 0;
                }
                _ => {}
            }
        }
    }

    fn apply(&mut self, ev: Event) {
        match ev {
            Event::Paragraph { paragraph, change } => {
                self.transcript.upsert(&paragraph);
                if change == Change::Hardened {
                    self.maybe_sent();
                }
            }
            Event::ParagraphRemoved { id } => {
                self.transcript.remove(&id);
                self.sending.retain(|s| *s != id);
            }
            Event::Level { rms, speaking, .. } => {
                self.rms = rms;
                self.speaking = speaking;
            }
        }
    }

    /// The paragraphs being spoken now (not sent or discarded).
    fn input_ids(&self) -> Vec<String> {
        self.transcript
            .paragraphs
            .iter()
            .filter(|p| !self.done.contains(&p.id) && !self.sending.contains(&p.id))
            .filter(|p| !p.text.trim().is_empty() || p.has_partial())
            .map(|p| p.id.clone())
            .collect()
    }

    /// The unsent text: the draft, then what was transcribed after it.
    fn input_text(&self) -> String {
        let ids = self.input_ids();
        self.draft
            .iter()
            .map(|d| d.trim())
            .chain(
                self.transcript
                    .paragraphs
                    .iter()
                    .filter(|p| ids.contains(&p.id))
                    .flat_map(|p| p.clips.iter().map(|c| c.text.trim())),
            )
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// `i`: edit the unsent text in a box. The mic is muted until it
    /// closes; speech still being decoded lands after the edit.
    fn open_editor(&mut self) {
        let ids = self.input_ids();
        let text: Vec<char> = self.input_text().chars().collect();
        if !ids.is_empty() {
            self.s.engine.break_paragraph();
        }
        self.editor = Some(Editor {
            cursor: text.len(),
            text,
            ids,
        });
    }

    /// Enter in the editor: its text replaces the unsent text it was
    /// opened with. Enter again sends it.
    fn apply_edit(&mut self) {
        let Some(ed) = self.editor.take() else {
            return;
        };
        self.done.extend(ed.ids);
        let text: String = ed.text.iter().collect();
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        self.draft = (!text.is_empty()).then_some(text);
    }

    /// Enter: send what has been said once its last pass is done.
    fn send(&mut self) {
        if !self.sending.is_empty() {
            return;
        }
        let ids = self.input_ids();
        if ids.is_empty() {
            match self.draft.take() {
                Some(text) => self.deliver(text),
                None => self.set_status("nothing to send yet"),
            }
            return;
        }
        self.s.engine.break_paragraph();
        self.sending = ids;
        self.maybe_sent();
    }

    /// Write the sent paragraphs once they have all hardened.
    fn maybe_sent(&mut self) {
        if self.sending.is_empty() {
            return;
        }
        let ps: Vec<_> = self
            .transcript
            .paragraphs
            .iter()
            .filter(|p| self.sending.contains(&p.id))
            .collect();
        if !ps.iter().all(|p| p.hardened) {
            return;
        }
        let text = self
            .draft
            .take()
            .into_iter()
            .chain(ps.iter().map(|p| p.text.trim().to_string()))
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        self.done.extend(self.sending.drain(..));
        if text.is_empty() {
            self.set_status("nothing was heard; not sent");
            return;
        }
        self.deliver(text);
    }

    /// Write `text` as the user's message and wait for the reply.
    fn deliver(&mut self, text: String) {
        self.cut_reply();
        match self.db.say(&text) {
            Ok(id) => {
                self.last_row = self.last_row.max(id);
                self.turns.push(Turn {
                    role: Role::User,
                    reply_to: None,
                    text,
                    cut: false,
                });
                self.awaiting = Some(id);
                self.more = true;
                self.scroll_back = 0;
            }
            Err(e) => self.set_status(format!("send failed: {e:#}")),
        }
    }

    /// Backspace: drop what has been said since the last send.
    fn discard(&mut self) {
        let ids = self.input_ids();
        if ids.is_empty() && self.draft.take().is_none() {
            return;
        }
        self.draft = None;
        if !ids.is_empty() {
            self.s.engine.break_paragraph();
        }
        self.done.extend(ids);
        self.set_status("discarded");
    }

    /// Space: hold the reply, or play it again from the start; between
    /// turns, pause the mic.
    fn space(&mut self) {
        if let Some(id) = self.awaiting {
            self.held = !self.held;
            if self.held {
                self.voice.set_paused(true);
                self.set_status("reply held: talk to interrupt, space to hear it again");
            } else {
                self.replay(id);
            }
        } else if self.playing.take().is_some() {
            self.voice.stop();
        } else {
            let was = self.s.paused.fetch_xor(true, Ordering::SeqCst);
            self.set_status(if was { "mic on" } else { "mic paused" });
        }
    }

    /// ↑ (`dir` -1) / ↓ (1): highlight the previous / next reply. ↓ past
    /// the last one goes back to the bottom.
    fn select(&mut self, dir: isize) {
        let replies: Vec<usize> = self
            .turns
            .iter()
            .enumerate()
            .filter(|(_, t)| t.role == Role::Assistant && !t.text.trim().is_empty())
            .map(|(i, _)| i)
            .collect();
        self.scroll_back = 0;
        self.selected = match (self.selected, dir < 0) {
            (None, true) => replies.last().copied(),
            (None, false) => None,
            (Some(cur), true) => replies
                .iter()
                .rev()
                .find(|&&i| i < cur)
                .or(replies.first())
                .copied(),
            (Some(cur), false) => replies.iter().find(|&&i| i > cur).copied(),
        };
    }

    /// Enter on a highlighted reply: play it again.
    fn play_turn(&mut self, i: usize) {
        if self.awaiting.is_some() {
            self.set_status("wait for this reply to finish (or space, then talk)");
            return;
        }
        let Some(t) = self.turns.get(i) else { return };
        let Some(key) = t.reply_to else { return };
        let text = t.text.clone();
        self.voice.stop();
        self.heard = None;
        self.playing = Some(key);
        self.voice.say(key, &text);
    }

    /// Speak the reply to `id`, as much as has arrived, from the top.
    /// Rows still to come are spoken after it as usual.
    fn replay(&mut self, id: i64) {
        self.voice.stop();
        self.heard = None;
        if let Some(t) = self
            .turns
            .iter()
            .rev()
            .find(|t| t.role == Role::Assistant && t.reply_to == Some(id))
        {
            self.voice.say(id, &t.text);
        }
    }

    /// Stop the reply in progress, if any, and tell the responder.
    fn cut_reply(&mut self) {
        let Some(id) = self.awaiting.take() else {
            return;
        };
        let audible = self.voice.busy();
        self.voice.stop();
        self.more = false;
        self.held = false;
        if let Some(t) = self
            .turns
            .iter_mut()
            .rev()
            .find(|t| t.role == Role::Assistant && t.reply_to == Some(id))
        {
            t.cut = true;
        }
        if let Err(e) = self.db.interrupt(id) {
            self.set_status(format!("interrupt failed: {e:#}"));
        } else if audible {
            self.set_status("interrupted");
        }
    }

    fn tick(&mut self) -> Result<()> {
        // Talking while the reply is held cuts it off.
        if self.held && !self.input_text().is_empty() {
            self.cut_reply();
        }
        for r in self.db.since(self.last_row)? {
            self.last_row = r.id;
            if r.role != Role::Assistant || r.reply_to.is_none() || r.reply_to != self.awaiting {
                continue;
            }
            push_reply(&mut self.turns, r.reply_to, &r.text);
            if let Some(key) = r.reply_to {
                self.voice.say(key, &r.text);
            }
            self.more = r.more;
            self.scroll_back = 0;
        }
        if let Some(h) = self.voice.heard() {
            self.heard = Some(h);
        }
        if self.awaiting.is_some() && !self.more && !self.voice.busy() {
            self.awaiting = None;
            self.held = false;
        }
        if self.playing.is_some() && !self.voice.busy() {
            self.playing = None;
        }
        // Half duplex: muted for the responder's whole turn unless held,
        // while an earlier reply is played again, and while typing.
        let open = self.editor.is_none()
            && (self.held || (self.awaiting.is_none() && self.playing.is_none()));
        if !open {
            self.open_since = None;
        } else if self.open_since.is_none() {
            self.open_since = Some(Instant::now());
        }
        let muted = self.open_since.is_none_or(|t| t.elapsed() < TAIL);
        self.s.muted.store(muted, Ordering::SeqCst);
        Ok(())
    }

    /// Write the whole chat, from the database, to the typed file.
    fn export_to(&mut self) {
        let Some((name, confirmed)) = self.export.clone() else {
            return;
        };
        let name = name.trim();
        if name.is_empty() {
            self.export = None;
            return;
        }
        let path = PathBuf::from(name);
        if path.exists() && !confirmed {
            self.export = Some((name.to_string(), true));
            self.set_status(format!("{name} exists: enter again to replace it"));
            return;
        }
        let title = self
            .db_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let result = self
            .db
            .since(0)
            .and_then(|rows| Ok(std::fs::write(&path, render(&title, &rows))?));
        match result {
            Ok(()) => {
                self.export = None;
                self.set_status(format!("exported to {name}"));
            }
            Err(e) => self.set_status(format!("export failed: {e:#}")),
        }
    }

    fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now()));
    }

    fn draw(&self, f: &mut Frame, name: &str) {
        let [header, body, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .areas(f.area());
        let busy = self.voice.busy();
        let state = if self.held {
            Span::styled(" ❚❚ REPLY HELD ", Style::new().black().on_yellow())
        } else if busy {
            Span::styled(" ♪ SPEAKING ", Style::new().black().bg(THEM))
        } else if !self.sending.is_empty() {
            Span::styled(" ⇡ SENDING ", Style::new().black().on_cyan())
        } else if self.awaiting.is_some() {
            Span::styled(" … WAITING ", Style::new().black().on_magenta())
        } else if self.s.paused.load(Ordering::Relaxed) {
            Span::styled(" ❚❚ MIC PAUSED ", Style::new().black().on_yellow())
        } else {
            Span::styled(" ◉ LISTENING ", Style::new().black().on_green())
        };
        f.render_widget(
            Line::from(vec![
                state,
                Span::raw(format!("  {}", self.s.device())),
                format!("  → {name}").dark_gray(),
            ]),
            header,
        );

        let width = (body.width.saturating_sub(2) as usize).max(GUTTER + 10);
        let mut lines: Vec<Line<'static>> = Vec::new();
        // The highlighted reply's [start, end) lines.
        let mut range = None;
        for (i, t) in self.turns.iter().enumerate() {
            let (label, color) = match t.role {
                Role::User => ("You", YOU),
                Role::Assistant => ("Reply", THEM),
            };
            let label_style = if self.selected == Some(i) {
                Style::new().black().bg(color).bold()
            } else {
                Style::new().fg(color).bold()
            };
            let start = lines.len();
            wrap_words(label, label_style, self.turn_words(t), width, &mut lines);
            if self.selected == Some(i) {
                range = Some((start, lines.len()));
            }
            lines.push(Line::default());
        }
        if !self.sending.is_empty() {
            let text = self
                .transcript
                .paragraphs
                .iter()
                .filter(|p| self.sending.contains(&p.id))
                .map(|p| p.text.trim())
                .collect::<Vec<_>>()
                .join(" ");
            wrap(
                "You",
                Style::new().fg(YOU),
                &text,
                Style::new().dark_gray(),
                width,
                &mut lines,
            );
            lines.push(Line::default());
        }
        let input = self.input_line(width);
        lines.extend(input);
        let visible = body.height.saturating_sub(2) as usize;
        let mut top = lines
            .len()
            .saturating_sub(visible)
            .saturating_sub(self.scroll_back as usize);
        // Keep the highlighted reply on screen.
        if let Some((start, end)) = range {
            if start < top {
                top = start;
            } else if end > top + visible {
                top = end.saturating_sub(visible).min(start);
            }
        }
        let title = if self.selected.is_some() || self.scroll_back > 0 {
            " Chat (esc back to the bottom) "
        } else {
            " Chat "
        };
        f.render_widget(
            Paragraph::new(Text::from(lines))
                .block(Block::bordered().title(title))
                .scroll((top as u16, 0)),
            body,
        );
        if let Some(ed) = &self.editor {
            ed.draw(f, body);
        }

        let [meter_line, help_line] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(footer);
        let db = if self.rms > 0.0 {
            20.0 * self.rms.log10()
        } else {
            -90.0
        };
        let filled = (((db + 60.0) / 60.0).clamp(0.0, 1.0) * 16.0).round() as usize;
        let muted = self.s.muted.load(Ordering::Relaxed);
        let mut meter = vec![
            Span::raw(" mic "),
            if muted && self.editor.is_some() {
                "muted while typing ".dark_gray()
            } else if muted {
                "muted: their turn ".dark_gray()
            } else {
                Span::styled(
                    format!("{}{}", "█".repeat(filled), "░".repeat(16 - filled)),
                    Style::new().fg(if self.speaking {
                        Color::Green
                    } else {
                        Color::Gray
                    }),
                )
            },
        ];
        if let Some((msg, _)) = &self.status {
            meter.push(format!("  {msg}").yellow());
        }
        f.render_widget(Line::from(meter), meter_line);
        if let Some((name, _)) = &self.export {
            f.render_widget(
                Line::from(vec![
                    " export to: ".yellow(),
                    Span::raw(name.clone()),
                    Span::styled("▌", Style::new().magenta()),
                    "   enter save · esc cancel".dark_gray(),
                ]),
                help_line,
            );
            return;
        }
        let keys = if self.selected.is_some() {
            " ↑↓ replies · enter play it again · space stop · esc back to the bottom"
        } else if self.scroll_back > 0 {
            " pgup/pgdn scroll · ↑↓ replies · esc back to the bottom"
        } else if self.playing.is_some() {
            " space stop · ↑↓ replies · q quit"
        } else if self.held {
            " talk to interrupt · space hear it again · q quit"
        } else if self.awaiting.is_some() {
            " space hold the reply (then talk to interrupt) · q quit"
        } else {
            " enter send · backspace discard · space pause mic · ↑↓ replies · i type/edit · e export · q quit"
        };
        f.render_widget(Line::from(keys.dark_gray()), help_line);
    }

    /// A turn's words, styled. The reply being spoken lights up like
    /// `--play`: heard words plain, the current one yellow, the rest dim.
    fn turn_words(&self, t: &Turn) -> Vec<(String, Style)> {
        let base = if t.cut {
            Style::new().add_modifier(Modifier::DIM)
        } else {
            Style::new()
        };
        let live = t.role == Role::Assistant
            && !t.cut
            && t.reply_to.is_some()
            && (t.reply_to == self.awaiting || t.reply_to == self.playing);
        let at = match self.heard {
            Some((key, i)) if Some(key) == t.reply_to => Some(i),
            _ => None,
        };
        let sounding = self.voice.heard().is_some();
        let mut words: Vec<(String, Style)> = t
            .text
            .split_whitespace()
            .enumerate()
            .map(|(i, w)| {
                let style = match (live, at) {
                    (false, _) => base,
                    (true, Some(a)) if i < a => base,
                    (true, Some(a)) if i == a && sounding => {
                        Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                    }
                    (true, Some(a)) if i == a => base,
                    (true, _) => Style::new().fg(Color::DarkGray),
                };
                (w.to_string(), style)
            })
            .collect();
        if t.cut {
            words.push(("—".into(), base));
        }
        words
    }

    /// What is being said now, with a cursor.
    fn input_line(&self, width: usize) -> Vec<Line<'static>> {
        let ids = self.input_ids();
        let mut words: Vec<(String, Style)> = self
            .draft
            .iter()
            .flat_map(|d| d.split_whitespace())
            .map(|w| (w.to_string(), Style::new()))
            .collect();
        for p in self
            .transcript
            .paragraphs
            .iter()
            .filter(|p| ids.contains(&p.id))
        {
            for c in &p.clips {
                let style = if c.stage == Stage::Partial {
                    Style::new().dark_gray().italic()
                } else {
                    Style::new()
                };
                words.extend(c.text.split_whitespace().map(|w| (w.to_string(), style)));
            }
        }
        words.push(("▌".into(), Style::new().magenta()));
        let mut out = Vec::new();
        wrap_words(
            "You",
            Style::new().fg(YOU).add_modifier(Modifier::DIM),
            words,
            width,
            &mut out,
        );
        out
    }
}

/// The chat as scribe markdown, `**[hh:mm:ss] You:** …` per turn (times
/// from the first message), so `scribe -p` can read it back in two
/// voices. A reply that was cut off ends in a dash.
fn render(title: &str, rows: &[Row]) -> String {
    let parse = |at: &str| chrono::NaiveDateTime::parse_from_str(at, "%Y-%m-%dT%H:%M:%S%.fZ").ok();
    let start = rows.first().and_then(|r| parse(&r.at));
    let subtitle = match start {
        Some(t) => format!("Chat · {} UTC", t.format("%Y-%m-%d %H:%M")),
        None => "Chat".into(),
    };
    let mut out = crate::markdown::header(title, &subtitle);
    // (role, reply_to, start ms, text, cut)
    let mut turns: Vec<(Role, Option<i64>, u64, String, bool)> = Vec::new();
    for r in rows {
        let ms = match (start, parse(&r.at)) {
            (Some(s), Some(t)) => (t - s).num_milliseconds().max(0) as u64,
            _ => 0,
        };
        let text = r.text.trim();
        match (r.role, r.kind) {
            (Role::User, Kind::Say) => turns.push((Role::User, None, ms, text.into(), false)),
            (Role::User, Kind::Interrupt) => {
                if let Some(t) = turns
                    .iter_mut()
                    .rev()
                    .find(|t| t.0 == Role::Assistant && t.1 == r.reply_to)
                {
                    t.4 = true;
                }
            }
            (Role::Assistant, _) => match turns.last_mut() {
                Some(t) if t.0 == Role::Assistant && t.1 == r.reply_to => {
                    if !text.is_empty() {
                        t.3 = format!("{} {text}", t.3).trim().to_string();
                    }
                }
                _ => turns.push((Role::Assistant, r.reply_to, ms, text.into(), false)),
            },
        }
    }
    for (role, _, ms, text, cut) in turns {
        if text.is_empty() {
            continue;
        }
        let who = match role {
            Role::User => "You",
            Role::Assistant => "Reply",
        };
        let dash = if cut { " —" } else { "" };
        let line = format!(
            "**[{}] {who}:** {text}{dash}",
            crate::markdown::timestamp(ms)
        );
        out.push_str(&format!("\n{}\n", crate::markdown::wrap(&line)));
    }
    out
}

fn wrap(
    label: &str,
    label_style: Style,
    text: &str,
    style: Style,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    let words = text
        .split_whitespace()
        .map(|w| (w.to_string(), style))
        .collect();
    wrap_words(label, label_style, words, width, out);
}

/// Word-wrap behind a `label:` gutter.
fn wrap_words(
    label: &str,
    label_style: Style,
    words: Vec<(String, Style)>,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    let text_w = width.saturating_sub(GUTTER).max(10);
    let mut line: Vec<Span<'static>> = vec![Span::styled(
        format!("{:<w$}", format!("{label}:"), w = GUTTER),
        label_style,
    )];
    let mut used = 0;
    for (w, style) in words {
        let ww = w.width();
        if used > 0 && used + 1 + ww > text_w {
            out.push(Line::from(std::mem::take(&mut line)));
            line.push(Span::raw(" ".repeat(GUTTER)));
            used = 0;
        }
        if used > 0 {
            line.push(Span::raw(" "));
            used += 1;
        }
        used += ww;
        line.push(Span::styled(w, style));
    }
    out.push(Line::from(line));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_markdown_scribe_can_play() {
        let row = |id, role, kind, reply_to, text: &str, at: &str| Row {
            id,
            role,
            kind,
            reply_to,
            text: text.into(),
            more: false,
            at: at.into(),
        };
        let rows = [
            row(
                1,
                Role::User,
                Kind::Say,
                None,
                "Hi there.",
                "2026-10-06T17:00:00.000Z",
            ),
            row(
                2,
                Role::Assistant,
                Kind::Say,
                Some(1),
                "Hello.",
                "2026-10-06T17:00:02.500Z",
            ),
            row(
                3,
                Role::Assistant,
                Kind::Say,
                Some(1),
                "And more.",
                "2026-10-06T17:00:03.000Z",
            ),
            row(
                4,
                Role::User,
                Kind::Interrupt,
                Some(1),
                "",
                "2026-10-06T17:00:04.000Z",
            ),
            row(
                5,
                Role::User,
                Kind::Say,
                None,
                "Stop.",
                "2026-10-06T17:01:05.000Z",
            ),
        ];
        let md = render("talk", &rows);
        assert!(md.starts_with("# talk\n\n*Chat · 2026-10-06 17:00 UTC*\n"));
        assert!(md.contains("\n**[00:00:00] You:** Hi there.\n"));
        assert!(md.contains("\n**[00:00:02] Reply:** Hello. And more. —\n"));
        assert!(md.contains("\n**[00:01:05] You:** Stop.\n"));
        let paras = crate::speak::paragraphs(&md);
        assert_eq!(paras.len(), 3);
        assert_eq!(paras[1].speaker.as_deref(), Some("Reply"));
    }

    #[test]
    fn editor_types_and_moves() {
        let mut ed = Editor {
            text: "helo".chars().collect(),
            cursor: 4,
            ids: Vec::new(),
        };
        ed.key(KeyCode::Left, false);
        ed.key(KeyCode::Char('l'), false);
        assert_eq!(ed.text.iter().collect::<String>(), "hello");
        ed.key(KeyCode::End, false);
        for c in " q".chars() {
            assert_eq!(ed.key(KeyCode::Char(c), false), None);
        }
        ed.key(KeyCode::Backspace, false);
        assert_eq!(ed.text.iter().collect::<String>(), "hello ");
        assert_eq!(ed.key(KeyCode::Char('q'), true), Some(false));
        assert_eq!(ed.key(KeyCode::Esc, false), Some(false));
        assert_eq!(ed.key(KeyCode::Enter, false), Some(true));
    }

    #[test]
    fn reply_chunks_join_one_turn() {
        let mut turns = Vec::new();
        push_reply(&mut turns, Some(1), "Hello.");
        push_reply(&mut turns, Some(1), "");
        push_reply(&mut turns, Some(1), "More.");
        push_reply(&mut turns, Some(2), "Next.");
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].text, "Hello. More.");
        assert_eq!(turns[1].reply_to, Some(2));
    }
}
