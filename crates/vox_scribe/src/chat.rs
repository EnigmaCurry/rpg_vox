//! `--chat NAME`: talk with a responder through NAME.db (see
//! [`crate::chatdb`]). What you say is held until Enter sends it. Then
//! it's the responder's turn: the mic is muted (half duplex, so it
//! doesn't hear the reply) until the reply has been read aloud and the
//! responder says it has no more. Space holds the reply and opens the
//! mic: start talking and the reply is cut off, or press space again to
//! hear the rest.

use std::collections::HashSet;
use std::path::Path;
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

use crate::chatdb::{Db, Kind, Role};
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
    /// Space during the responder's turn: reply paused, mic open.
    held: bool,
    /// When the mic was last wanted open, for [`TAIL`].
    open_since: Option<Instant>,
    rms: f32,
    speaking: bool,
    scroll_back: u16,
    status: Option<(String, Instant)>,
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
        held: false,
        open_since: Some(Instant::now()),
        rms: 0.0,
        speaking: false,
        scroll_back: 0,
        status: None,
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
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('c') if ctrl => return Ok(()),
                KeyCode::Enter => self.send(),
                KeyCode::Backspace => self.discard(),
                KeyCode::Char(' ') => self.space(),
                KeyCode::PageUp => self.scroll_back = self.scroll_back.saturating_add(10),
                KeyCode::PageDown => self.scroll_back = self.scroll_back.saturating_sub(10),
                KeyCode::End => self.scroll_back = 0,
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

    fn input_text(&self) -> String {
        let ids = self.input_ids();
        self.transcript
            .paragraphs
            .iter()
            .filter(|p| ids.contains(&p.id))
            .flat_map(|p| p.clips.iter().map(|c| c.text.trim()))
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Enter: send what has been said once its last pass is done.
    fn send(&mut self) {
        if !self.sending.is_empty() {
            return;
        }
        let ids = self.input_ids();
        if ids.is_empty() {
            self.set_status("nothing to send yet");
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
        let text = ps
            .iter()
            .map(|p| p.text.trim())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        self.done.extend(self.sending.drain(..));
        if text.is_empty() {
            self.set_status("nothing was heard; not sent");
            return;
        }
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
        if ids.is_empty() {
            return;
        }
        self.s.engine.break_paragraph();
        self.done.extend(ids);
        self.set_status("discarded");
    }

    /// Space: hold or resume the reply; between turns, pause the mic.
    fn space(&mut self) {
        if self.awaiting.is_some() {
            self.held = !self.held;
            self.voice.set_paused(self.held);
            if self.held {
                self.set_status("reply held: talk to interrupt, space to resume");
            }
        } else {
            let was = self.s.paused.fetch_xor(true, Ordering::SeqCst);
            self.set_status(if was { "mic on" } else { "mic paused" });
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
            self.voice.say(&r.text);
            self.more = r.more;
            self.scroll_back = 0;
        }
        if self.awaiting.is_some() && !self.more && !self.voice.busy() {
            self.awaiting = None;
            self.held = false;
        }
        // Half duplex: muted for the responder's whole turn unless held.
        let open = self.awaiting.is_none() || self.held;
        if !open {
            self.open_since = None;
        } else if self.open_since.is_none() {
            self.open_since = Some(Instant::now());
        }
        let muted = self.open_since.is_none_or(|t| t.elapsed() < TAIL);
        self.s.muted.store(muted, Ordering::SeqCst);
        Ok(())
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
        for t in &self.turns {
            let (label, color) = match t.role {
                Role::User => ("You", YOU),
                Role::Assistant => ("Reply", THEM),
            };
            let mut text = t.text.clone();
            if t.cut {
                text.push_str(" —");
            }
            let style = if t.cut {
                Style::new().add_modifier(Modifier::DIM)
            } else {
                Style::new()
            };
            wrap(
                label,
                Style::new().fg(color).bold(),
                &text,
                style,
                width,
                &mut lines,
            );
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
        let top = lines
            .len()
            .saturating_sub(visible)
            .saturating_sub(self.scroll_back as usize);
        f.render_widget(
            Paragraph::new(Text::from(lines))
                .block(Block::bordered().title(" Chat "))
                .scroll((top as u16, 0)),
            body,
        );

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
            if muted {
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
        let keys = if self.held {
            " talk to interrupt · space resume the reply · q quit"
        } else if self.awaiting.is_some() {
            " space hold the reply (then talk to interrupt) · q quit"
        } else {
            " enter send · backspace discard · space pause mic · pgup/pgdn scroll · q quit"
        };
        f.render_widget(Line::from(keys.dark_gray()), help_line);
    }

    /// What is being said now, with a cursor.
    fn input_line(&self, width: usize) -> Vec<Line<'static>> {
        let ids = self.input_ids();
        let mut words: Vec<(String, Style)> = Vec::new();
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
