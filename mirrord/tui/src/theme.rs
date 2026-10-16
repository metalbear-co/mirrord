//! MetalBear brand palette, the styles built from it, and how it degrades on terminals that
//! cannot show it.
//!
//! The colours are taken from the MetalBear website. Only foreground colours and
//! selection backgrounds are used, so the application keeps whatever background
//! the terminal already has.
//!
//! Colours carry the same meaning on every screen, so a glance says where to look:
//! - [`MINT`]: done, ready, safe
//! - [`AMBER`]: a guess worth checking, or something still missing
//! - [`CORAL`]: failed, or a choice that affects other people (taking over a target)
//! - [`TEAL`] and [`SKY`]: things in the cluster, targets and namespaces
//! - [`INDIGO`]: the brand, keys and focus
//!
//! Most text stays [`LAVENDER`] or [`MUTED`]; colour goes on borders, short values and chips, so
//! the screen does not turn into a rainbow.
//!
//! Screens always draw with the 24-bit colours below. [`adapt`] runs over every finished frame and
//! swaps them for named terminal colours, or drops colour entirely, when the terminal or the user
//! asks for that, so no screen has to think about colour support.

use std::sync::LazyLock;

use ratatui::{
    buffer::Buffer,
    style::{Color, Modifier, Style},
};

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
/// Cluster namespaces. Not part of the marketing palette, picked to sit next to it.
pub const SKY: Color = Color::Rgb(0x5B, 0x9D, 0xF3);
/// Cluster targets: the workloads a session runs against.
pub const TEAL: Color = Color::Rgb(0x4D, 0xB6, 0xB0);
/// Error accent. Not part of the marketing palette, picked to sit next to it.
pub const CORAL: Color = Color::Rgb(0xF2, 0x77, 0x7A);
/// De-emphasised text.
pub const MUTED: Color = Color::Rgb(0x88, 0x88, 0x99);
/// Lighter grey for secondary text that still has to be read, such as paths and hints.
pub const GREY: Color = Color::Rgb(0xAC, 0xAC, 0xAC);
/// Dim purple, between [`INDIGO`] and the background, for secondary fills.
pub const FILL_DIM: Color = Color::Rgb(0x4F, 0x4A, 0xB8);
/// Dark border colour for unfocused panes and separators.
pub const BORDER_DIM: Color = Color::Rgb(0x3E, 0x3C, 0x5E);
/// Brand ink, for solid fills behind light text and dark text on light chips.
pub const INK: Color = Color::Rgb(0x23, 0x21, 0x41);

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

/// A short label on a solid colour, such as ` READY `, for the one thing on a screen that should
/// catch the eye first.
pub fn chip(color: Color) -> Style {
    Style::default()
        .fg(INK)
        .bg(color)
        .add_modifier(Modifier::BOLD)
}

/// How much colour the terminal gets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ColorSupport {
    /// 24-bit colour: the palette is drawn as is.
    TrueColor,
    /// Only the 16 named colours, which every terminal has and every theme defines.
    Named,
    /// No colour at all, because the user set `NO_COLOR`. Bold, dim and reverse video stay.
    None,
}

/// Decided once per process. There is no quick, reliable way to ask a terminal what it supports,
/// so the environment decides, first match wins:
/// - `NO_COLOR` set to anything non-empty turns colour off (<https://no-color.org>)
/// - `COLORTERM` containing `truecolor` or `24bit` keeps 24-bit colour, the same signal bat, delta
///   and starship go by
/// - otherwise named colours, since `Color::Rgb` on a terminal without 24-bit support is drawn as
///   one flat grey, with bold text falling back to plain white on top of it
static COLOR_SUPPORT: LazyLock<ColorSupport> = LazyLock::new(|| {
    if std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty()) {
        return ColorSupport::None;
    }
    let truecolor = std::env::var("COLORTERM").is_ok_and(|value| {
        let value = value.to_ascii_lowercase();
        value.contains("truecolor") || value.contains("24bit")
    });
    if truecolor {
        ColorSupport::TrueColor
    } else {
        ColorSupport::Named
    }
});

/// Named colours for the palette, chosen by hand so every colour that carries a meaning (healthy,
/// warning, failed, focus, each Preview Environments phase) keeps one distinct from the others.
/// Anything not listed falls back to the nearest named colour.
const NAMED: &[((u8, u8, u8), Color)] = &[
    ((0x75, 0x6D, 0xF3), Color::LightMagenta), // INDIGO
    ((0xA8, 0xA5, 0xF7), Color::LightBlue),    // INDIGO_LIGHT
    ((0xE4, 0xE3, 0xFD), Color::White),        // LAVENDER
    ((0x2E, 0x2A, 0x5E), Color::Blue),         // DEEP, the selection background
    ((0xFF, 0xCB, 0x7D), Color::LightYellow),  // AMBER
    ((0x7D, 0xD3, 0xA8), Color::LightGreen),   // MINT
    ((0xF2, 0x77, 0x7A), Color::LightRed),     // CORAL
    ((0x88, 0x88, 0x99), Color::DarkGray),     // MUTED
    ((0xAC, 0xAC, 0xAC), Color::Gray),         // GREY
    ((0x4F, 0x4A, 0xB8), Color::Blue),         // FILL_DIM
    ((0x3E, 0x3C, 0x5E), Color::DarkGray),     // BORDER_DIM
    ((0x23, 0x21, 0x41), Color::Black),        // INK
    ((0x7A, 0x78, 0xA0), Color::Magenta),      // Preview Environments: Initializing
    ((0xFF, 0x5F, 0x57), Color::LightRed),     // Preview Environments: Failed
    ((0xB8, 0x96, 0x5E), Color::Yellow),       // Preview Environments: Paused
    ((0x5B, 0x9D, 0xF3), Color::LightBlue),    // SKY
    ((0x4D, 0xB6, 0xB0), Color::LightCyan),    // TEAL
    ((0x2B, 0x27, 0x57), Color::DarkGray),     // Preview Environments: focused card tint
];

/// The 16 named colours with the RGB values xterm draws them in, for finding the nearest one.
const ANSI: &[((u8, u8, u8), Color)] = &[
    ((0x00, 0x00, 0x00), Color::Black),
    ((0xCD, 0x00, 0x00), Color::Red),
    ((0x00, 0xCD, 0x00), Color::Green),
    ((0xCD, 0xCD, 0x00), Color::Yellow),
    ((0x00, 0x00, 0xEE), Color::Blue),
    ((0xCD, 0x00, 0xCD), Color::Magenta),
    ((0x00, 0xCD, 0xCD), Color::Cyan),
    ((0xE5, 0xE5, 0xE5), Color::Gray),
    ((0x7F, 0x7F, 0x7F), Color::DarkGray),
    ((0xFF, 0x00, 0x00), Color::LightRed),
    ((0x00, 0xFF, 0x00), Color::LightGreen),
    ((0xFF, 0xFF, 0x00), Color::LightYellow),
    ((0x5C, 0x5C, 0xFF), Color::LightBlue),
    ((0xFF, 0x00, 0xFF), Color::LightMagenta),
    ((0x00, 0xFF, 0xFF), Color::LightCyan),
    ((0xFF, 0xFF, 0xFF), Color::White),
];

/// The named colour to draw `color` with on a terminal without 24-bit colour.
fn named(color: Color) -> Color {
    let Color::Rgb(r, g, b) = color else {
        return color;
    };
    if let Some((_, named)) = NAMED.iter().find(|(rgb, _)| *rgb == (r, g, b)) {
        return *named;
    }

    let distance = |(r2, g2, b2): (u8, u8, u8)| {
        let channel = |a: u8, b: u8| (i32::from(a) - i32::from(b)).pow(2);
        channel(r, r2) + channel(g, g2) + channel(b, b2)
    };
    ANSI.iter()
        .min_by_key(|(rgb, _)| distance(*rgb))
        .map_or(Color::Reset, |(_, named)| *named)
}

/// Rewrites a finished frame for the terminal's colour support. Called once per frame, after
/// every screen and dialog has drawn.
///
/// This covers the Terminal screen's shell output too: a program inside it asking for 24-bit
/// colour gets the same treatment as the interface around it.
pub fn adapt(buffer: &mut Buffer) {
    adapt_to(buffer, *COLOR_SUPPORT);
}

fn adapt_to(buffer: &mut Buffer, support: ColorSupport) {
    match support {
        ColorSupport::TrueColor => {}
        ColorSupport::Named => {
            for cell in &mut buffer.content {
                cell.fg = named(cell.fg);
                cell.bg = named(cell.bg);
            }
        }
        ColorSupport::None => {
            for cell in &mut buffer.content {
                cell.fg = Color::Reset;
                cell.bg = Color::Reset;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{buffer::Buffer, layout::Rect, style::Color};

    use super::{AMBER, ColorSupport, INDIGO, adapt_to, named};

    #[test]
    fn palette_colours_keep_distinct_named_colours() {
        assert_eq!(named(INDIGO), Color::LightMagenta);
        assert_eq!(named(AMBER), Color::LightYellow);
        // Not in the palette: the nearest named colour.
        assert_eq!(named(Color::Rgb(0xF0, 0x10, 0x10)), Color::LightRed);
        // Already named: left alone.
        assert_eq!(named(Color::Cyan), Color::Cyan);
    }

    #[test]
    fn no_color_drops_colour_but_keeps_text() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 3, 1));
        buffer.set_string(
            0,
            0,
            "abc",
            ratatui::style::Style::default().fg(INDIGO).bg(AMBER),
        );

        adapt_to(&mut buffer, ColorSupport::None);

        let cell = buffer.content.first().expect("the buffer has 3 cells");
        assert_eq!(
            (cell.fg, cell.bg, cell.symbol()),
            (Color::Reset, Color::Reset, "a")
        );
    }
}
