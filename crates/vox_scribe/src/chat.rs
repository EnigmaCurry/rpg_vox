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

use crate::chatdb::{Conversation, Db, Kind, Role, Row};
use crate::tui::{normalize_key, Screen};
use crate::voice::Voice;
use crate::Session;

const STATUS_TTL: Duration = Duration::from_secs(3);
/// Keep the mic muted this long after the reply stops sounding, so its
/// tail isn't heard as speech.
const TAIL: Duration = Duration::from_millis(300);
/// Auto mode (`m`): send once there has been no speech for this long.
const AUTO_SEND: Duration = Duration::from_millis(1500);
const YOU: Color = Color::Cyan;
const THEM: Color = Color::Indexed(117);
const GUTTER: usize = 11;
/// [`Voice`] key for the `v` menu's sample line (no reply has it).
const SAMPLE: i64 = -1;

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
    /// The responder is working on a reply and has said nothing yet
    /// (its last row was empty with more to come).
    thinking: bool,
    /// For animating the thinking spinner.
    born: Instant,
    /// `m`: send on a pause in speech instead of on Enter.
    auto: bool,
    /// When speech was last heard, or the unsent text last changed.
    last_voice: Instant,
    last_input: String,
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
    /// The Kokoro voice replies are read in.
    voice_name: String,
    /// The `v` voice menu, while open: the highlighted entry.
    voices: Option<usize>,
    /// The conversation shown, and its title.
    conv: i64,
    conv_title: String,
    /// The `c` list, while open: the conversations and the highlighted
    /// entry (0 is "new conversation").
    convs: Option<(Vec<Conversation>, usize)>,
    /// `t`: the new title being typed.
    retitle: Option<String>,
}

/// The saved voice of `conv`'s replies.
pub fn saved_voice(db: &Db, conv: i64) -> Result<Option<String>> {
    db.setting(&format!("voice.{conv}"))
}

/// A file-name-safe version of `title`: `2026-10-06 17:09` → `2026-10-06-17-09`.
fn slug(title: &str) -> String {
    let s: String = title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    s.split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Voices offered by `v`: Kokoro's English ones (American and British).
fn voice_choices() -> Vec<&'static str> {
    vox_transcribe::tts::VOICES
        .iter()
        .copied()
        .filter(|v| matches!(&v[..2], "af" | "am" | "bf" | "bm"))
        .collect()
}

/// "af_heart" → "Heart · American female".
fn voice_label(v: &str) -> String {
    let accent = if v.starts_with('a') {
        "American"
    } else {
        "British"
    };
    let sex = if v.as_bytes().get(1) == Some(&b'f') {
        "female"
    } else {
        "male"
    };
    let name = v.get(3..).unwrap_or(v);
    let mut chars = name.chars();
    let name = chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect::<String>())
        .unwrap_or_default();
    format!("{name} · {accent} {sex}")
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

pub fn run(
    mut screen: Screen,
    session: Session,
    db_path: &Path,
    voice: Voice,
    voice_name: String,
) -> Result<()> {
    let db = Db::open(db_path)?;
    let last_row = db.last_id()?;
    let conv = db.current()?;
    db.set_current(conv)?;
    let conv_title = db.title(conv)?.unwrap_or_default();
    let mut chat = Chat {
        s: &session,
        voice,
        voice_name,
        voices: None,
        conv,
        conv_title,
        convs: None,
        retitle: None,
        transcript: Transcript::default(),
        turns: history(&db, conv)?,
        db,
        last_row,
        awaiting: None,
        more: false,
        thinking: false,
        born: Instant::now(),
        auto: false,
        last_voice: Instant::now(),
        last_input: String::new(),
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

/// The turns of conversation `conv` so far.
fn history(db: &Db, conv: i64) -> Result<Vec<Turn>> {
    let mut turns: Vec<Turn> = Vec::new();
    for r in db.rows_in(conv)? {
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
    if text.is_empty() {
        return;
    }
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
            if let Some(sel) = self.voices {
                let n = voice_choices().len();
                match key.code {
                    KeyCode::Char('c') if ctrl => return Ok(()),
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('v') => self.voices = None,
                    KeyCode::Up | KeyCode::Char('k') => self.voices = Some(sel.saturating_sub(1)),
                    KeyCode::Down | KeyCode::Char('j') => self.voices = Some((sel + 1).min(n - 1)),
                    KeyCode::PageUp => self.voices = Some(sel.saturating_sub(8)),
                    KeyCode::PageDown => self.voices = Some((sel + 8).min(n - 1)),
                    KeyCode::Home => self.voices = Some(0),
                    KeyCode::End => self.voices = Some(n - 1),
                    KeyCode::Enter => self.choose_voice(sel),
                    _ => {}
                }
                continue;
            }
            if let Some(ed) = self.editor.as_mut() {
                match ed.key(key.code, ctrl) {
                    Some(true) => self.apply_edit(),
                    Some(false) => self.editor = None,
                    None => {}
                }
                continue;
            }
            if let Some((list, sel)) = self.convs.as_mut() {
                let n = list.len() + 1;
                match key.code {
                    KeyCode::Char('c') if ctrl => return Ok(()),
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('c') => self.convs = None,
                    KeyCode::Up | KeyCode::Char('k') => *sel = sel.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => *sel = (*sel + 1).min(n - 1),
                    KeyCode::PageUp => *sel = sel.saturating_sub(8),
                    KeyCode::PageDown => *sel = (*sel + 8).min(n - 1),
                    KeyCode::Home => *sel = 0,
                    KeyCode::End => *sel = n - 1,
                    KeyCode::Enter => {
                        let pick = match *sel {
                            0 => None,
                            i => list.get(i - 1).map(|c| c.id),
                        };
                        self.convs = None;
                        self.open_conversation(pick);
                    }
                    _ => {}
                }
                continue;
            }
            if let Some(title) = self.retitle.as_mut() {
                match key.code {
                    KeyCode::Char('c') if ctrl => return Ok(()),
                    KeyCode::Esc => self.retitle = None,
                    KeyCode::Enter => self.save_title(),
                    KeyCode::Backspace => {
                        title.pop();
                    }
                    KeyCode::Char(c) => title.push(c),
                    _ => {}
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
                KeyCode::Char('c') if !ctrl => {
                    if self.awaiting.is_some() {
                        self.set_status("wait for this reply to finish first");
                    } else {
                        match self.db.conversations() {
                            Ok(list) => {
                                let at = list.iter().position(|c| c.id == self.conv);
                                self.convs = Some((list, at.map_or(0, |i| i + 1)));
                            }
                            Err(e) => self.set_status(format!("listing failed: {e:#}")),
                        }
                    }
                }
                KeyCode::Char('t') => self.retitle = Some(self.conv_title.clone()),
                KeyCode::Char('v') => {
                    let at = voice_choices().iter().position(|v| *v == self.voice_name);
                    self.voices = Some(at.unwrap_or(0));
                }
                KeyCode::Char('m') => {
                    self.auto = !self.auto;
                    self.set_status(if self.auto {
                        "auto: a pause in speech sends it"
                    } else {
                        "manual: enter sends"
                    });
                }
                KeyCode::Char('e') => {
                    // NAME-<conversation title>.md, beside the database.
                    let stem = self
                        .db_path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let name = self
                        .db_path
                        .with_file_name(format!("{stem}-{}.md", slug(&self.conv_title)));
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
                if speaking {
                    self.last_voice = Instant::now();
                }
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

    /// Enter in the `v` menu: reply in this voice from now on, in this
    /// conversation (saved in its database), and say a line in it.
    fn choose_voice(&mut self, sel: usize) {
        let Some(&name) = voice_choices().get(sel) else {
            return;
        };
        let Some(sid) = vox_transcribe::tts::voice_id(name) else {
            return;
        };
        self.voice.set_voice(sid);
        self.voice_name = name.to_string();
        match self.db.set_setting(&format!("voice.{}", self.conv), name) {
            Ok(()) => self.set_status(format!("replies now in {}", voice_label(name))),
            Err(e) => self.set_status(format!("saving the voice failed: {e:#}")),
        }
        // A sample, unless a reply is under way.
        if self.awaiting.is_none() {
            self.voice.stop();
            self.heard = None;
            self.playing = Some(SAMPLE);
            self.voice
                .say(SAMPLE, "Hello. This is how I'll sound from now on.");
        }
    }

    /// Show conversation `pick`, or a new one; it opens next time too.
    fn open_conversation(&mut self, pick: Option<i64>) {
        let result = (|| -> Result<(i64, String, Vec<Turn>)> {
            let id = match pick {
                Some(id) => id,
                None => self.db.new_conversation()?,
            };
            self.db.set_current(id)?;
            let title = self.db.title(id)?.unwrap_or_default();
            Ok((id, title, history(&self.db, id)?))
        })();
        match result {
            Ok((id, title, turns)) => {
                self.voice.stop();
                self.playing = None;
                self.heard = None;
                self.selected = None;
                self.scroll_back = 0;
                self.conv = id;
                self.turns = turns;
                self.set_status(if pick.is_some() {
                    format!("opened {title}")
                } else {
                    format!("new conversation: {title} (t to rename)")
                });
                self.conv_title = title;
                // Its own reply voice, if one was chosen.
                if let Ok(Some(name)) = saved_voice(&self.db, id) {
                    if let Some(sid) = vox_transcribe::tts::voice_id(&name) {
                        self.voice.set_voice(sid);
                        self.voice_name = name;
                    }
                }
            }
            Err(e) => self.set_status(format!("opening failed: {e:#}")),
        }
    }

    /// Enter in the `t` prompt.
    fn save_title(&mut self) {
        let Some(title) = self.retitle.take() else {
            return;
        };
        let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
        if title.is_empty() {
            return;
        }
        match self.db.rename(self.conv, &title) {
            Ok(()) => {
                self.conv_title = title;
                self.set_status("renamed");
            }
            Err(e) => self.set_status(format!("rename failed: {e:#}")),
        }
    }

    /// The `c` list over the chat.
    fn draw_convs(&self, f: &mut Frame, area: ratatui::layout::Rect) {
        use ratatui::widgets::Clear;
        let Some((list, sel)) = &self.convs else {
            return;
        };
        let w = 64.min(area.width);
        let h = (list.len() as u16 + 3).min(area.height);
        let rect = ratatui::layout::Rect {
            x: area.x + area.width.saturating_sub(w) / 2,
            y: area.y + area.height.saturating_sub(h) / 2,
            width: w,
            height: h,
        };
        let inner = w.saturating_sub(2) as usize;
        let mut rows: Vec<Line> = vec![Line::from("  + New conversation".to_string()).fg(THEM)];
        for c in list {
            let mark = if c.id == self.conv { "● " } else { "  " };
            let right = format!("{} · {} ", c.last, c.messages);
            let room = inner.saturating_sub(mark.width() + right.width() + 1);
            let mut title: String = c.title.chars().collect();
            while title.width() > room {
                title.pop();
            }
            let pad = inner.saturating_sub(mark.width() + title.width() + right.width());
            rows.push(Line::from(vec![
                Span::raw(format!("{mark}{title}{}", " ".repeat(pad))),
                Span::styled(right, Style::new().dark_gray()),
            ]));
        }
        let visible = h.saturating_sub(2) as usize;
        let first = sel.saturating_sub(visible.saturating_sub(1));
        let lines: Vec<Line> = rows
            .into_iter()
            .enumerate()
            .skip(first)
            .take(visible)
            .map(|(i, l)| {
                if i == *sel {
                    l.style(Style::new().add_modifier(Modifier::REVERSED))
                } else {
                    l
                }
            })
            .collect();
        f.render_widget(Clear, rect);
        f.render_widget(
            Paragraph::new(lines).block(
                Block::bordered()
                    .title(" Conversations ")
                    .title_bottom(Line::from(" ↑↓ · enter open · esc close ").dark_gray()),
            ),
            rect,
        );
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
        match self.db.say(self.conv, &text) {
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
                self.thinking = false;
                self.scroll_back = 0;
            }
            Err(e) => self.set_status(format!("send failed: {e:#}")),
        }
    }

    /// Backspace: drop the last sentence of the unsent text. Holding it
    /// down repeats, so it eats back through the whole message. What's
    /// left becomes the draft (as if typed with `i`).
    fn discard(&mut self) {
        let text = self.input_text();
        if text.is_empty() {
            return;
        }
        let ids = self.input_ids();
        if !ids.is_empty() {
            self.s.engine.break_paragraph();
        }
        self.done.extend(ids);
        self.draft = drop_last_sentence(&text);
        if self.draft.is_none() {
            self.set_status("discarded");
        }
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
        self.thinking = false;
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
        let input = self.input_text();
        // Talking while the reply is held cuts it off.
        if self.held && !input.is_empty() {
            self.cut_reply();
        }
        if input != self.last_input {
            self.last_voice = Instant::now();
            self.last_input = input;
        }
        // Auto mode: send what was said once the speaker pauses. Typed
        // text alone still waits for Enter.
        if self.auto
            && self.sending.is_empty()
            && self.editor.is_none()
            && !self.speaking
            && self.last_voice.elapsed() >= AUTO_SEND
            && !self.input_ids().is_empty()
            && !self
                .transcript
                .paragraphs
                .iter()
                .any(|p| self.input_ids().contains(&p.id) && p.has_partial())
        {
            self.send();
        }
        for r in self.db.since(self.last_row)? {
            self.last_row = r.id;
            if r.role != Role::Assistant || r.reply_to.is_none() || r.reply_to != self.awaiting {
                continue;
            }
            // An empty row with more to come: the responder is thinking.
            self.thinking = r.more && r.text.trim().is_empty();
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
        let title = self.conv_title.clone();
        let result = self
            .db
            .rows_in(self.conv)
            .and_then(|rows| Ok(std::fs::write(&path, render(&title, &rows))?));
        match result {
            Ok(()) => {
                self.export = None;
                self.set_status(format!("exported to {name}"));
            }
            Err(e) => self.set_status(format!("export failed: {e:#}")),
        }
    }

    /// The `v` menu over the chat.
    fn draw_voices(&self, f: &mut Frame, area: ratatui::layout::Rect, sel: usize) {
        use ratatui::widgets::Clear;
        let choices = voice_choices();
        let h = (choices.len() as u16 + 2).min(area.height);
        let w = 44.min(area.width);
        let rect = ratatui::layout::Rect {
            x: area.x + area.width.saturating_sub(w) / 2,
            y: area.y + area.height.saturating_sub(h) / 2,
            width: w,
            height: h,
        };
        let rows = h.saturating_sub(2) as usize;
        let first = sel.saturating_sub(rows.saturating_sub(1));
        let lines: Vec<Line> = choices
            .iter()
            .enumerate()
            .skip(first)
            .take(rows)
            .map(|(i, v)| {
                let mark = if *v == self.voice_name { "● " } else { "  " };
                let line = Line::from(format!("{mark}{}", voice_label(v)));
                if i == sel {
                    line.style(Style::new().add_modifier(Modifier::REVERSED))
                } else {
                    line
                }
            })
            .collect();
        f.render_widget(Clear, rect);
        f.render_widget(
            Paragraph::new(lines).block(
                Block::bordered()
                    .title(" Reply voice ")
                    .title_bottom(Line::from(" ↑↓ · enter choose · esc close ").dark_gray()),
            ),
            rect,
        );
    }

    /// A frame of the thinking spinner, from the clock.
    fn spinner(&self) -> &'static str {
        const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        FRAMES[(self.born.elapsed().as_millis() / 80) as usize % FRAMES.len()]
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
        } else if self.awaiting.is_some() && self.thinking {
            Span::styled(
                format!(" {} THINKING ", self.spinner()),
                Style::new().black().bg(THEM),
            )
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
                if self.auto {
                    Span::styled(" ¶ AUTO ", Style::new().black().on_gray())
                } else {
                    Span::styled(" ¶ MANUAL ", Style::new().black().on_magenta())
                },
                Span::raw(format!("  {}", self.s.device())),
                format!("  → {name}").dark_gray(),
            ]),
            header,
        );

        let width = (body.width.saturating_sub(2) as usize).max(GUTTER + 10);
        let mut lines: Vec<Line<'static>> = Vec::new();
        // The highlighted reply's [start, end) lines, counted from the
        // question it answers so that stays in view too (from the top for
        // the first reply).
        let mut range = None;
        let mut prev_start = 0;
        let mut seen_reply = false;
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
                let from = match (seen_reply, i.checked_sub(1).map(|j| self.turns[j].role)) {
                    (false, _) => 0,
                    (true, Some(Role::User)) => prev_start,
                    _ => start,
                };
                range = Some((from, lines.len()));
            }
            if t.role == Role::Assistant && !t.text.trim().is_empty() {
                seen_reply = true;
            }
            prev_start = start;
            lines.push(Line::default());
        }
        if self.awaiting.is_some() && self.thinking {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<w$}", "Reply:", w = GUTTER),
                    Style::new().fg(THEM).bold(),
                ),
                Span::styled(
                    format!("{} thinking…", self.spinner()),
                    Style::new().fg(THEM),
                ),
            ]));
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
            format!(" {} (esc back to the bottom) ", self.conv_title)
        } else {
            format!(" {} ", self.conv_title)
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
        if let Some(sel) = self.voices {
            self.draw_voices(f, body, sel);
        }
        self.draw_convs(f, body);

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
        if let Some(title) = &self.retitle {
            f.render_widget(
                Line::from(vec![
                    " title: ".yellow(),
                    Span::raw(title.clone()),
                    Span::styled("▌", Style::new().magenta()),
                    "   enter save · esc cancel".dark_gray(),
                ]),
                help_line,
            );
            return;
        }
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
            // The full list, or a shorter one that fits a narrow terminal.
            let full = " enter send · backspace drop a sentence · space pause mic · m mode · ↑↓ replies · i type · c chats · t title · v voice · e export · q quit";
            let short = " enter send · ⌫ sentence · space mic · m mode · i type · c chats · t title · v voice · e export · q quit";
            if full.width() <= help_line.width as usize {
                full
            } else {
                short
            }
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

/// `text` without its last sentence, or `None` if nothing is left.
fn drop_last_sentence(text: &str) -> Option<String> {
    let mut sentences = crate::speak::sentences(text);
    sentences.pop();
    let rest = sentences.join(" ");
    (!rest.is_empty()).then_some(rest)
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
            conversation: 1,
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
    fn titles_make_file_names() {
        assert_eq!(slug("2026-10-06 17:09"), "2026-10-06-17-09");
        assert_eq!(slug("Dragons & Goblins!"), "dragons-goblins");
    }

    #[test]
    fn backspace_drops_a_sentence() {
        assert_eq!(
            drop_last_sentence("One. Two? Three and").as_deref(),
            Some("One. Two?")
        );
        assert_eq!(drop_last_sentence("One. Two?").as_deref(), Some("One."));
        assert_eq!(drop_last_sentence("One."), None);
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
