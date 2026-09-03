use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::Text,
    widgets::Paragraph,
};

use crate::{context::Context, helpers::centered, screens::Screen, theme};

/// ASCII art logo options, in the order of preference.
///
/// Logo is shown on the home screen.
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
