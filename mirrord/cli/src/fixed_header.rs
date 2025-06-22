use std::{io::{self, Write}, sync::mpsc::{Receiver, Sender}};

use crossterm::{event::{self, Event, KeyCode}, execute, terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen}};
use ratatui::{backend::CrosstermBackend, Terminal, widgets::Paragraph, layout::Rect, prelude::Frame};
use tracing_subscriber::fmt::MakeWriter;

pub struct ChannelMakeWriter {
    tx: Sender<String>,
}

impl ChannelMakeWriter {
    pub fn new(tx: Sender<String>) -> Self {
        Self { tx }
    }
}

impl<'a> MakeWriter<'a> for ChannelMakeWriter {
    type Writer = ChannelWriter;
    fn make_writer(&'a self) -> Self::Writer {
        ChannelWriter { tx: self.tx.clone() }
    }
}

pub struct ChannelWriter {
    tx: Sender<String>,
}

/// Draws the fixed header and log area onto the provided frame.
pub fn draw_ui(f: &mut Frame<'_>, messages: &[String], logs: &[String], offset: usize) {
    let size = f.size();
    let header_height = messages.len() as u16;
    for (idx, line) in messages.iter().enumerate() {
        let area = Rect::new(0, idx as u16, size.width, 1);
        f.render_widget(Paragraph::new(line.as_str()), area);
    }
    let log_area = Rect::new(0, header_height, size.width, size.height.saturating_sub(header_height));
    let start = logs.len().saturating_sub(offset + log_area.height as usize);
    let end = logs.len().saturating_sub(offset);
    let text = logs[start..end].join("\n");
    f.render_widget(Paragraph::new(text), log_area);
}

impl Write for ChannelWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _ = self.tx.send(String::from_utf8_lossy(buf).into_owned());
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn run(messages: Vec<String>, rx: Receiver<String>) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut logs: Vec<String> = Vec::new();
    let mut offset = 0usize;

    loop {
        terminal.draw(|f| draw_ui(f, &messages, &logs, offset))?;

        while let Ok(line) = rx.try_recv() {
            logs.push(line);
        }

        if event::poll(std::time::Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Up => {
                        if offset + 1 < logs.len() {
                            offset += 1;
                        }
                    }
                    KeyCode::Down => {
                        if offset > 0 {
                            offset -= 1;
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('q') => break,
                    _ => {}
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::draw_ui;
    use crate::config::{Cli, OutputMode};
    use crate::logging::init_tracing_registry;
    use ratatui::{backend::TestBackend, Terminal};
    use clap::Parser;
    use drain::channel as drain_channel;

    #[test]
    fn header_single_line() {
        let backend = TestBackend::new(20, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        let messages = vec!["header".to_string()];
        let logs: Vec<String> = vec![];
        terminal.draw(|f| draw_ui(f, &messages, &logs, 0)).unwrap();
        terminal.backend().assert_buffer_lines([
            "header              ",
            "                    ",
            "                    ",
        ]);
    }

    #[test]
    fn header_two_lines() {
        let backend = TestBackend::new(20, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        let messages = vec!["line1".to_string(), "line2".to_string()];
        let logs: Vec<String> = vec![];
        terminal.draw(|f| draw_ui(f, &messages, &logs, 0)).unwrap();
        terminal.backend().assert_buffer_lines([
            "line1               ",
            "line2               ",
            "                    ",
            "                    ",
        ]);
    }

    #[test]
    fn header_static_while_logs_scroll() {
        let backend = TestBackend::new(20, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        let messages = vec!["hdr".to_string()];
        let mut logs = vec!["first".to_string()];
        terminal.draw(|f| draw_ui(f, &messages, &logs, 0)).unwrap();
        logs.push("second".to_string());
        terminal.draw(|f| draw_ui(f, &messages, &logs, 0)).unwrap();
        terminal.backend().assert_buffer_lines([
            "hdr                 ",
            "first               ",
            "second              ",
            "                    ",
        ]);
    }

    #[test]
    fn scrolling_doesnt_move_header() {
        let backend = TestBackend::new(20, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        let messages = vec!["hdr".to_string()];
        let logs = vec!["one".to_string(), "two".to_string(), "three".to_string()];
        terminal.draw(|f| draw_ui(f, &messages, &logs, 1)).unwrap();
        terminal.backend().assert_buffer_lines([
            "hdr                 ",
            "one                 ",
            "two                 ",
            "                    ",
        ]);
    }

    #[test]
    fn parse_fixed_header_args() {
        let cli = Cli::try_parse_from([
            "mirrord",
            "exec",
            "--output-mode",
            "fixed-header",
            "--header",
            "a",
            "--header",
            "b",
            "echo",
        ])
        .unwrap();
        if let crate::config::Commands::Exec(args) = cli.commands {
            assert_eq!(args.params.output_mode, Some(OutputMode::FixedHeader));
            assert_eq!(args.params.header.unwrap(), vec!["a", "b"]);
        } else {
            panic!("expected exec command");
        }
    }

    #[tokio::test]
    async fn standard_mode_disables_fixed_header() {
        let cli = Cli::try_parse_from([
            "mirrord",
            "exec",
            "--output-mode",
            "standard",
            "echo",
        ])
        .unwrap();
        let (_, watch) = drain_channel();
        let handle = init_tracing_registry(&cli.commands, watch).await.unwrap();
        assert!(handle.is_none());
    }
}
