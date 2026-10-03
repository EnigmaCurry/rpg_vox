//! How speaker labels look: "Speaker A", or a name the user gave it
//! (`--speakers Alice,Bob`, or `n` in the TUI), in a colour of its own. The
//! colours follow the ASS styles in `vox_transcribe::subtitle`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use ratatui::style::Color;
use vox_transcribe::speaker::{index, label};

const PALETTE: [Color; 8] = [
    Color::Indexed(213), // pink
    Color::Indexed(114), // green
    Color::Indexed(215), // orange
    Color::Indexed(141), // purple
    Color::Indexed(80),  // teal
    Color::Indexed(222), // sand
    Color::Indexed(203), // red
    Color::Indexed(153), // sky
];

/// Label → name, for the whole process (one session at a time).
fn names() -> &'static Mutex<HashMap<String, String>> {
    static NAMES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    NAMES.get_or_init(Default::default)
}

/// Set when a name changes after files may already hold the old one.
static RENAMED: AtomicBool = AtomicBool::new(false);

/// Name speakers A, B, … in order (`--speakers Alice,Bob`).
pub fn preset(list: &[String]) {
    let mut n = names().lock().expect("names lock");
    for (i, name) in list.iter().enumerate() {
        let name = clean(name);
        if !name.is_empty() {
            n.insert(label(i), name);
        }
    }
}

/// Rename `label`; an empty name restores "Speaker A".
pub fn rename(label: &str, name: &str) {
    let name = clean(name);
    let mut n = names().lock().expect("names lock");
    if name.is_empty() {
        n.remove(label);
    } else {
        n.insert(label.to_string(), name);
    }
    RENAMED.store(true, Ordering::SeqCst);
}

/// True if any speaker was renamed during the session.
pub fn renamed() -> bool {
    RENAMED.load(Ordering::SeqCst)
}

/// One line, no commas (they separate ASS fields), trimmed.
fn clean(name: &str) -> String {
    name.replace([',', '\n', '\r'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// "A" → its given name, else "Speaker A".
pub fn name(label: &str) -> String {
    names()
        .lock()
        .ok()
        .and_then(|n| n.get(label).cloned())
        .unwrap_or_else(|| format!("Speaker {label}"))
}

pub fn color(label: &str) -> Color {
    index(label).map(color_at).unwrap_or(Color::Gray)
}

/// The colour of the `i`th speaker.
pub fn color_at(i: usize) -> Color {
    PALETTE[i % PALETTE.len()]
}
