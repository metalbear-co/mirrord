//! MetalBear brand palette for the targets wizard.
//!
//! Source of truth is the dashboard palette in the operator repo
//! (`operator-dashboard/src/lib/palette.ts`, values from `@metalbear/ui`).
//! Colors are truecolor RGB; terminals without truecolor support approximate.
//! Scoped to this screen for now so other screens stay untouched; promote to
//! a shared module once another screen wants it.

use ratatui::style::Color;

/// Main Purple `#756DF3` - hero accent: selection highlight, focused pane
/// border, active wizard step, primary action hints.
pub const BRAND: Color = Color::Rgb(0x75, 0x6D, 0xF3);

/// Cream purple `#E4E3FD` - emphasized text on the dark terminal background
/// (headings, selected row text).
pub const TEXT_EMPHASIS: Color = Color::Rgb(0xE4, 0xE3, 0xFD);

/// Dim purple `#4F4AB8` - secondary fills, sits between brand and background.
pub const FILL_DIM: Color = Color::Rgb(0x4F, 0x4A, 0xB8);

/// Dark border `#3E3C5E` - unfocused pane borders and separators.
pub const BORDER_DIM: Color = Color::Rgb(0x3E, 0x3C, 0x5E);

/// Grey `#ACACAC` - muted/secondary text (paths, hints, disabled items).
pub const TEXT_MUTED: Color = Color::Rgb(0xAC, 0xAC, 0xAC);

/// Yellow `#FFCB7D` - warnings and action-required only, never decoration.
pub const WARNING: Color = Color::Rgb(0xFF, 0xCB, 0x7D);

/// Mint `#7DD3A8` - healthy/finished states (matches the app palette).
pub const SUCCESS: Color = Color::Rgb(0x7D, 0xD3, 0xA8);

/// Dark purple `#232141` - solid fills behind light text (headers).
pub const FILL_HEAVY: Color = Color::Rgb(0x23, 0x21, 0x41);

use super::browser::TargetKind;

/// Per-kind badge color so every target kind is distinct at a glance.
///
/// Hues share the brand palette's saturation family; yellow stays reserved
/// for warnings.
pub fn kind_color(kind: TargetKind) -> Color {
    match kind {
        TargetKind::Deployment => BRAND,
        TargetKind::Rollout => Color::Rgb(0xF3, 0x6D, 0xD3),
        TargetKind::StatefulSet => Color::Rgb(0x6D, 0xF3, 0xB1),
        TargetKind::Pod => Color::Rgb(0x6D, 0xC7, 0xF3),
        TargetKind::CronJob => Color::Rgb(0xF3, 0xA1, 0x6D),
        TargetKind::Job => Color::Rgb(0xE8, 0xF3, 0x6D),
        TargetKind::Service => Color::Rgb(0x6D, 0xF3, 0xEE),
        TargetKind::ReplicaSet => Color::Rgb(0xB5, 0x6D, 0xF3),
    }
}
