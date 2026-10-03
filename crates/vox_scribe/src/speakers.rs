//! How speaker labels look: "Speaker A" in a colour of its own. The
//! colours follow the ASS styles in `vox_transcribe::subtitle`.

use ratatui::style::Color;
use vox_transcribe::speaker::index;

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

/// "A" → "Speaker A".
pub fn name(label: &str) -> String {
    format!("Speaker {label}")
}

pub fn color(label: &str) -> Color {
    index(label)
        .map(|i| PALETTE[i % PALETTE.len()])
        .unwrap_or(Color::Gray)
}
