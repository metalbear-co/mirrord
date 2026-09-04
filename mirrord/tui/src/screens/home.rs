use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::Text,
    widgets::Paragraph,
};

use crate::{context::Context, helpers::centered, screens::Screen, theme};

/// Logo options for the home screen, largest first: the first one the area can hold is the
/// one drawn. The wider one carries the wordmark, which needs the width to stay legible; the
/// narrower one is the mirror alone.
///
/// The art is built from half-block glyphs, so each line holds two rows of the image and the
/// logo has twice the vertical resolution of the character grid it occupies. It carries no
/// colour of its own - it is drawn in the brand colour over whatever background the terminal
/// already has, like the rest of the palette.
///
/// Regenerate with `scripts/render_tui_logo.sh`, which documents the exact invocations.
///
/// Lines are padded to a fixed width, so the art is rendered left aligned in an
/// area of exactly that width. Centring the lines individually would skew it.
const LOGOS: [&str; 2] = [
    include_str!("../../resources/logo-big"),
    include_str!("../../resources/logo-small"),
];

/// The home screen.
#[derive(Debug, Default)]
pub struct HomeScreen {}

impl Screen for HomeScreen {
    fn new(_context: Context) -> Self {
        Self {}
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        for logo in LOGOS {
            let logo = Text::raw(logo.trim_end_matches('\n'));
            let logo_width = logo.width() as u16;
            let logo_height = logo.height() as u16;
            // One blank line between the logo and the greeting.
            let total_height = logo_height + 2;

            if area.width < logo_width || area.height < total_height {
                // Area too small to display this logo.
                continue;
            }

            let [logo_area, _, welcome_area] = Layout::vertical([
                Constraint::Length(logo_height),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .areas(centered(
                area,
                Constraint::Length(logo_width),
                Constraint::Length(total_height),
            ));
            frame.render_widget(
                Paragraph::new(logo).style(Style::default().fg(theme::INDIGO)),
                logo_area,
            );
            frame.render_widget(Paragraph::new("Welcome!").centered(), welcome_area);
            return;
        }

        frame.render_widget(
            Paragraph::new("Welcome!").centered(),
            centered(area, Constraint::Fill(1), Constraint::Length(1)),
        );
    }
}
