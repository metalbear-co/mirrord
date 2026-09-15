//! MetalBear brand palette and the styles built from it.
//!
//! The colours are taken from the MetalBear website. Only foreground colours and
//! selection backgrounds are used, so the application keeps whatever background
//! the terminal already has.

use ratatui::style::{Color, Modifier, Style};

/// Primary brand colour.
pub const INDIGO: Color = Color::Rgb(0x75, 0x6D, 0xF3);
/// Lighter tint of [`INDIGO`], used for secondary text.
pub const INDIGO_LIGHT: Color = Color::Rgb(0xA8, 0xA5, 0xF7);
/// Palest tint of [`INDIGO`], used for text on brand-coloured backgrounds.
pub const LAVENDER: Color = Color::Rgb(0xE4, 0xE3, 0xFD);
/// Brand dark, used as the selection background.
pub const DEEP: Color = Color::Rgb(0x2E, 0x2A, 0x5E);
/// Brand accent, used for warnings and in-progress states.
pub const AMBER: Color = Color::Rgb(0xFF, 0xCB, 0x7D);
/// Brand accent, used for healthy states.
pub const MINT: Color = Color::Rgb(0x7D, 0xD3, 0xA8);
/// Error accent. Not part of the marketing palette, picked to sit next to it.
pub const CORAL: Color = Color::Rgb(0xF2, 0x77, 0x7A);
/// De-emphasised text.
pub const MUTED: Color = Color::Rgb(0x88, 0x88, 0x99);

/// Style for a panel title.
pub fn title() -> Style {
    Style::default().fg(INDIGO).add_modifier(Modifier::BOLD)
}

/// Style for a panel border.
pub fn border() -> Style {
    Style::default().fg(INDIGO)
}

/// Style for a table header row.
pub fn table_header() -> Style {
    Style::default()
        .fg(INDIGO_LIGHT)
        .add_modifier(Modifier::BOLD)
}

/// Style for the selected table row.
pub fn selection() -> Style {
    Style::default()
        .bg(DEEP)
        .fg(LAVENDER)
        .add_modifier(Modifier::BOLD)
}

/// Style for de-emphasised text, such as key hints and placeholder values.
pub fn muted() -> Style {
    Style::default().fg(MUTED)
}

/// Style for an error message.
pub fn error() -> Style {
    Style::default().fg(CORAL)
}

/// Style for a warning message.
pub fn warning() -> Style {
    Style::default().fg(AMBER)
}
