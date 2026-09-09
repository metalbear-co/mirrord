//! The side panel: the mirrord sessions running under the pane's shell.
//!
//! The panel is only drawn when there is at least one session to put in it — see [`super::split`] —
//! so there is no empty state here. All it needs is the sessions themselves.

use k8s_openapi::jiff::Timestamp;
use mirrord_session_monitor_client::SessionInfo;
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, BorderType, Padding, Paragraph},
};

use crate::{helpers::ellipsize, screens::terminal::sessions::primary_process, theme};

/// Draws `sessions` into `area`. Never called with none.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    sessions: &[SessionInfo],
    updated_at: Option<Timestamp>,
) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::border())
        .padding(Padding::horizontal(1))
        .title(Span::styled(" mirrord ", theme::title()))
        .title_bottom(
            Line::styled(
                format!(" {} ", footer(sessions, updated_at)),
                theme::muted(),
            )
            .right_aligned(),
        );

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // The panel is narrow and does not scroll, so the text is measured against the width it will
    // actually be drawn at rather than wrapped.
    let width = usize::from(inner.width);
    let mut lines = Vec::new();

    for session in sessions {
        if !lines.is_empty() {
            lines.push(Line::default());
        }

        lines.push(Line::styled(
            ellipsize(&session.target, width),
            theme::title(),
        ));

        let scope = match (session.namespace.as_deref(), session.context.as_deref()) {
            (Some(namespace), Some(context)) => format!("{context}/{namespace}"),
            (Some(namespace), None) => namespace.to_owned(),
            (None, Some(context)) => context.to_owned(),
            (None, None) => "(default)".to_owned(),
        };
        lines.push(Line::styled(ellipsize(&scope, width), theme::muted()));

        for subscription in &session.port_subscriptions {
            lines.push(Line::from_iter([
                Span::styled(
                    format!("{} ", subscription.mode),
                    match subscription.mode.as_str() {
                        "steal" => theme::warning(),
                        _ => Style::default().fg(theme::MINT),
                    },
                ),
                Span::styled(format!(":{}", subscription.port), theme::muted()),
            ]));
        }

        if let Some(process) = primary_process(session) {
            let command = match process.cmdline.is_empty() {
                false => process.cmdline.join(" "),
                true => process.process_name.clone(),
            };
            lines.push(Line::styled(ellipsize(&command, width), theme::muted()));
        }

        lines.push(Line::styled(
            ellipsize(
                &format!(
                    "{} \u{2022} up {}",
                    match session.is_operator {
                        true => "operator",
                        false => "oss",
                    },
                    uptime(&session.started_at),
                ),
                width,
            ),
            theme::muted(),
        ));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

/// The line under the border: how many sessions there are and how fresh the reading is.
fn footer(sessions: &[SessionInfo], updated_at: Option<Timestamp>) -> String {
    let count = sessions.len();

    match updated_at {
        Some(updated_at) => format!(
            "{count} \u{2022} {}s ago",
            Timestamp::now().duration_since(updated_at).as_secs().max(0),
        ),
        None => count.to_string(),
    }
}

/// How long a session has been running, from the RFC 3339 timestamp the registry reports.
fn uptime(started_at: &str) -> String {
    let Ok(started_at) = started_at.parse::<Timestamp>() else {
        return "unknown".to_owned();
    };

    let seconds = Timestamp::now().duration_since(started_at).as_secs().max(0);

    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;

    if seconds < MINUTE {
        format!("{seconds}s")
    } else if seconds < HOUR {
        format!("{}m{}s", seconds / MINUTE, seconds % MINUTE)
    } else if seconds < DAY {
        format!("{}h{}m", seconds / HOUR, seconds % HOUR / MINUTE)
    } else {
        format!("{}d{}h", seconds / DAY, seconds % DAY / HOUR)
    }
}

#[cfg(test)]
mod tests {
    use mirrord_session_monitor_client::ProcessInfo;
    use mirrord_session_monitor_protocol::PortSubscription;
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    /// Renders the panel at the width it is drawn at in the application.
    fn render(sessions: &[SessionInfo], height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(
            crate::screens::terminal::PANEL_WIDTH,
            height,
        ))
        .unwrap();

        terminal
            .draw(|frame| draw(frame, frame.area(), sessions, Some(Timestamp::now())))
            .unwrap();

        let buffer = terminal.backend().buffer().clone();

        (0..buffer.area.height)
            .map(|row| {
                (0..buffer.area.width)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn session(target: &str, processes: Vec<ProcessInfo>) -> SessionInfo {
        SessionInfo {
            session_id: "abc".to_owned(),
            key: None,
            target: target.to_owned(),
            namespace: Some("staging".to_owned()),
            context: None,
            started_at: (Timestamp::now() - std::time::Duration::from_secs(65)).to_string(),
            mirrord_version: "3.249.0".to_owned(),
            is_operator: true,
            processes,
            port_subscriptions: vec![PortSubscription {
                port: 8080,
                mode: "steal".to_owned(),
            }],
            config: serde_json::Value::Null,
        }
    }

    #[test]
    fn a_session_is_drawn_with_its_scope_ports_command_and_uptime() {
        let sessions = [session(
            "deployment/api",
            vec![ProcessInfo {
                pid: 800,
                parent_pid: None,
                process_name: "node".to_owned(),
                cmdline: vec!["node".to_owned(), "server.js".to_owned()],
            }],
        )];

        let rendered = render(&sessions, 10);

        assert!(rendered.contains("deployment/api"), "{rendered}");
        assert!(rendered.contains("staging"), "{rendered}");
        assert!(rendered.contains("steal :8080"), "{rendered}");
        assert!(rendered.contains("node server.js"), "{rendered}");
        assert!(rendered.contains("operator \u{2022} up 1m5s"), "{rendered}");
    }

    #[test]
    fn a_target_too_long_for_the_panel_is_cut_rather_than_wrapped() {
        let sessions = [session(
            "deployment/a-very-long-deployment-name-indeed",
            Vec::new(),
        )];

        let rendered = render(&sessions, 10);

        assert!(
            rendered.contains("deployment/a-very-long-deploy\u{2026}"),
            "the target should be ellipsized to the panel width, got:\n{rendered}",
        );
    }

    /// Several sessions share the panel, in the order they were given.
    #[test]
    fn every_session_gets_its_own_block() {
        let sessions = [
            session("deployment/first", Vec::new()),
            session("deployment/second", Vec::new()),
        ];

        let rendered = render(&sessions, 14);

        let first = rendered
            .find("deployment/first")
            .expect("first session missing");
        let second = rendered
            .find("deployment/second")
            .expect("second session missing");

        assert!(
            first < second,
            "sessions should keep their order:\n{rendered}"
        );
        assert!(rendered.contains("2 \u{2022}"), "{rendered}");
    }

    #[test]
    fn uptime_is_read_from_the_registrys_rfc3339_timestamps() {
        let started_at = Timestamp::now() - std::time::Duration::from_secs(90);

        assert_eq!(uptime(&started_at.to_string()), "1m30s");
    }

    #[test]
    fn an_unparsable_timestamp_does_not_panic() {
        assert_eq!(uptime("whenever"), "unknown");
    }
}
