//! `a` in the TUI: pick which inputs and apps to transcribe, one or
//! several at once. Each source is its own speaker, as with repeated
//! `-d` / `-a` on the command line.

use std::sync::atomic::Ordering;

use ratatui::crossterm::event::KeyCode;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;
use vox_audio::{AppInfo, OpenOptions};

use crate::Session;

pub struct SourcesMenu {
    rows: Vec<Row>,
    cursor: usize,
    /// --diarize: one source at a time.
    single: bool,
}

struct Row {
    label: String,
    kind: &'static str,
    opts: OpenOptions,
    /// The session slot already capturing this, if any.
    slot: Option<usize>,
    checked: bool,
    app: Option<AppInfo>,
}

/// What a key in the menu asks for.
pub enum Action {
    None,
    Close,
    /// Apply: keep these slots, open these new inputs.
    Apply(Vec<usize>, Vec<OpenOptions>),
}

impl SourcesMenu {
    /// The inputs and apps there are now, with the session's current
    /// sources ticked.
    pub fn open(s: &Session) -> Self {
        let backend = vox_audio::default_backend().ok();
        let devices = backend
            .as_ref()
            .and_then(|b| b.list_devices().ok())
            .unwrap_or_default();
        let apps = backend
            .as_ref()
            .and_then(|b| b.list_apps().ok())
            .unwrap_or_default();
        let mut rows = vec![Row {
            label: "System default input".into(),
            kind: "input",
            opts: OpenOptions::default(),
            slot: None,
            checked: false,
            app: None,
        }];
        for d in devices {
            rows.push(Row {
                label: if d.is_default {
                    format!("{} (default)", d.name)
                } else {
                    d.name
                },
                kind: "input",
                opts: OpenOptions {
                    device: Some(d.id),
                    ..Default::default()
                },
                slot: None,
                checked: false,
                app: None,
            });
        }
        // Apps playing sound first; a name shared by several processes
        // gets its PID.
        let mut apps = apps;
        apps.sort_by_key(|a| (!a.playing, a.name.to_lowercase()));
        let shared = |a: &AppInfo| apps.iter().filter(|b| b.name == a.name).count() > 1;
        let labels: Vec<String> = apps
            .iter()
            .map(|a| {
                if shared(a) {
                    format!("{} ({})", a.name, a.pid)
                } else {
                    a.name.clone()
                }
            })
            .collect();
        for (a, label) in apps.into_iter().zip(labels) {
            rows.push(Row {
                label,
                kind: if a.playing { "app ♪" } else { "app" },
                opts: OpenOptions {
                    app: Some(a.pid.to_string()),
                    ..Default::default()
                },
                slot: None,
                checked: false,
                app: Some(a),
            });
        }
        // Tick what's on now; a source that's switched off still owns its
        // row, so picking it again brings back its speaker label.
        let slots = s.slots.lock().expect("slots lock");
        for on in [true, false] {
            for (i, slot) in slots.iter().enumerate() {
                if !slot.live || slot.off.load(Ordering::Relaxed) == on {
                    continue;
                }
                let o = slot.opts.lock().expect("opts lock").clone();
                match rows.iter().position(|r| r.slot.is_none() && r.is(&o)) {
                    Some(j) => {
                        rows[j].slot = Some(i);
                        rows[j].checked = on;
                    }
                    None if on => rows.push(Row {
                        label: slot.name.lock().expect("name lock").clone(),
                        kind: if o.virtual_sink {
                            "virtual sink"
                        } else {
                            "current"
                        },
                        opts: o,
                        slot: Some(i),
                        checked: true,
                        app: None,
                    }),
                    None => {}
                }
            }
        }
        let cursor = rows.iter().position(|r| r.checked).unwrap_or(0);
        Self {
            rows,
            cursor,
            single: s.diarize,
        }
    }

    pub fn key(&mut self, code: KeyCode) -> Action {
        let last = self.rows.len().saturating_sub(1);
        match code {
            KeyCode::Up | KeyCode::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.cursor = (self.cursor + 1).min(last),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = last,
            KeyCode::Char(' ') | KeyCode::Char('x') => {
                let was = self.rows[self.cursor].checked;
                if self.single {
                    for r in &mut self.rows {
                        r.checked = false;
                    }
                }
                self.rows[self.cursor].checked = !was || self.single;
            }
            KeyCode::Enter => {
                let picked = self.rows.iter().filter(|r| r.checked);
                let keep = picked.clone().filter_map(|r| r.slot).collect();
                let new = picked
                    .filter(|r| r.slot.is_none())
                    .map(|r| r.opts.clone())
                    .collect();
                return Action::Apply(keep, new);
            }
            KeyCode::Esc | KeyCode::Char('a') | KeyCode::Char('q') => return Action::Close,
            _ => {}
        }
        Action::None
    }

    pub fn draw(&self, f: &mut Frame, area: Rect) {
        let width = self
            .rows
            .iter()
            .map(|r| r.label.chars().count())
            .max()
            .unwrap_or(0);
        let lines: Vec<Line> = self
            .rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let tick = match (r.checked, self.single) {
                    (true, true) => "(•) ",
                    (false, true) => "( ) ",
                    (true, false) => "[x] ",
                    (false, false) => "[ ] ",
                };
                let line = Line::from(vec![
                    Span::raw(" "),
                    if r.checked {
                        tick.green().bold()
                    } else {
                        tick.into()
                    },
                    Span::raw(format!("{:width$}  ", r.label)),
                    Span::styled(r.kind, Style::new().dark_gray()),
                ]);
                if i == self.cursor {
                    line.style(Style::new().add_modifier(Modifier::REVERSED))
                } else {
                    line
                }
            })
            .collect();
        let help = if self.single {
            " ↑↓ move · space pick · enter apply · esc cancel "
        } else {
            " ↑↓ move · space toggle · enter apply · esc cancel "
        };
        let w = (lines.iter().map(|l| l.width()).max().unwrap_or(0) + 3)
            .max(help.chars().count() + 2)
            .min(area.width as usize) as u16;
        let h = (lines.len() as u16 + 2).min(area.height);
        let rect = Rect {
            x: area.x + area.width.saturating_sub(w) / 2,
            y: area.y + area.height.saturating_sub(h) / 2,
            width: w,
            height: h,
        };
        // Keep the cursor row in view.
        let visible = h.saturating_sub(2) as usize;
        let top = (self.cursor + 1).saturating_sub(visible) as u16;
        f.render_widget(Clear, rect);
        f.render_widget(
            Paragraph::new(lines).scroll((top, 0)).block(
                Block::bordered()
                    .title(" Audio sources ")
                    .title_bottom(Line::from(help).dark_gray()),
            ),
            rect,
        );
    }
}

impl Row {
    /// This row is what `o` captures.
    fn is(&self, o: &OpenOptions) -> bool {
        if o.virtual_sink {
            return false;
        }
        match (&o.device, &o.app) {
            (_, Some(want)) => self.app.as_ref().is_some_and(|a| a.matches(want)),
            (Some(d), None) => {
                self.opts.device.as_deref() == Some(d.as_str())
                    || (self.kind == "input"
                        && self.opts.device.is_some()
                        && self.label.to_lowercase().contains(&d.to_lowercase()))
            }
            (None, None) => self.kind == "input" && self.opts.device.is_none(),
        }
    }
}
