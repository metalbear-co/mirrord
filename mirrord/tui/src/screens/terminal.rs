//! A screen that hosts a real interactive shell.
//!
//! Two properties drive the whole design:
//!
//! - **The screen does not interpret keys.** While the shell has the keyboard, everything except
//!   the `Ctrl-B` prefix is re-encoded into terminal bytes (see [`input`]) and written to the pty,
//!   so `Ctrl-C`, `Ctrl-D`, `Ctrl-Z` and `Ctrl-L` reach the shell with their usual meaning — and
//!   the application's own global keys (`q`, `Tab`, ...) do not. `Ctrl-B` hands the keyboard back
//!   to the application; `Enter` gives it to the shell again.
//! - **The pane sizes the shell.** The pty is resized to the pane's inner rectangle, so `stty size`
//!   inside the shell reports the pane and not the window the application runs in. A fixed-width
//!   side panel (see [`panel`]) listing the mirrord sessions started from that shell splits the
//!   screen vertically — but only while there are sessions to list, so an ordinary shell gets the
//!   whole body and the panel takes its columns the moment a session appears.

use std::{
    collections::VecDeque,
    fmt,
    ops::ControlFlow,
    sync::{Arc, Mutex},
};

use portable_pty::CommandBuilder;
use ratatui::{
    Frame,
    crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers},
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::Span,
    widgets::{Block, BorderType, Paragraph},
};
use tui_term::widget::PseudoTerminal;

use crate::{
    context::Context,
    helpers::centered,
    screens::{
        Screen,
        terminal::{
            input::{encode_key, encode_paste},
            sessions::SessionWatcher,
        },
    },
};

mod input;
mod panel;
mod pty;
mod sessions;

/// Lines of history the emulator keeps for `C-b PageUp`.
const SCROLLBACK: usize = 1000;

/// Width of the session panel, wide enough for a `namespace/deployment-name` to survive the
/// ellipsis more often than not.
pub(super) const PANEL_WIDTH: u16 = 34;

/// Below this, the panel is dropped entirely rather than squeezing the shell into a column too
/// narrow to work in.
const MIN_SHELL_WIDTH: u16 = 40;

#[derive(Debug)]
pub enum Ev {
    Pty(Vec<u8>),
    PtyClosed,
}

struct State {
    parser: vt100::Parser,
    pty: pty::PtyHost,
    /// Rows and columns the child currently believes it has.
    size: (u16, u16),
    unfocused: bool,
    scrollback: usize,
    exited: Option<String>,
}

impl State {
    fn new(rows: u16, cols: u16, tx: std::sync::mpsc::Sender<Ev>) -> anyhow::Result<Self> {
        Ok(State {
            parser: vt100::Parser::new(rows, cols, SCROLLBACK),
            pty: pty::PtyHost::spawn(
                CommandBuilder::new_default_prog(),
                pty::PtyParams { rows, cols },
                tx,
            )?,
            size: (rows, cols),
            unfocused: false,
            scrollback: 0,
            exited: None,
        })
    }

    fn handle(&mut self, ev: Ev) -> anyhow::Result<()> {
        match ev {
            Ev::Pty(bytes) => self.parser.process(&bytes),
            Ev::PtyClosed => {
                let status = self.pty.wait()?;
                self.exited = Some(format!("exited: {}", status.exit_code()));
            }
        }

        Ok(())
    }

    fn on_key(&mut self, key: KeyEvent) -> anyhow::Result<ControlFlow<(), KeyEvent>> {
        if self.unfocused {
            return self.on_prefix_command(key);
        }

        if Self::is_prefix(&key) {
            self.unfocused = true;
            return Ok(ControlFlow::Break(()));
        }

        self.scroll_to_live();
        if let Some(bytes) = encode_key(key, self.parser.screen().application_cursor()) {
            self.pty.write(&bytes)?;
        }

        Ok(ControlFlow::Break(()))
    }

    fn is_prefix(key: &KeyEvent) -> bool {
        key.code == KeyCode::Char('b') && key.modifiers.contains(KeyModifiers::CONTROL)
    }

    fn on_prefix_command(&mut self, key: KeyEvent) -> anyhow::Result<ControlFlow<(), KeyEvent>> {
        let half_screen = usize::from(self.size.0) / 2;
        match key.code {
            KeyCode::Enter => {
                self.unfocused = false;
            }
            // `C-b C-b` types a literal C-b, the standard way to reach the prefix key itself.
            _ if Self::is_prefix(&key) => self.pty.write(&[0x02])?,
            KeyCode::Up => self.scroll(self.scrollback + 1),
            KeyCode::Down => self.scroll(self.scrollback.saturating_sub(1)),
            KeyCode::PageUp => self.scroll(self.scrollback + half_screen),
            KeyCode::PageDown => self.scroll(self.scrollback.saturating_sub(half_screen)),
            _ => return Ok(ControlFlow::Continue(key)),
        }

        Ok(ControlFlow::Break(()))
    }

    fn reconcile_size(&mut self, area: Rect) -> anyhow::Result<()> {
        let inner = pane_inner(area);
        let size = (inner.height, inner.width);
        // A pane too small to hold anything would mean a degenerate pty and screen.
        if size == self.size || inner.height == 0 || inner.width == 0 {
            return Ok(());
        }

        self.size = size;
        self.parser.screen_mut().set_size(size.0, size.1);
        self.pty.resize(size.0, size.1)
    }

    fn scroll(&mut self, lines: usize) {
        self.parser.screen_mut().set_scrollback(lines);
        // vt100 clamps to the history it actually has, so read back rather than trusting `lines`.
        self.scrollback = self.parser.screen().scrollback();
    }

    fn scroll_to_live(&mut self) {
        if self.scrollback != 0 {
            self.scroll(0);
        }
    }

    fn block(&self) -> Block<'static> {
        let (rows, cols) = self.size;
        let (title, style) = match (&self.exited, self.unfocused, self.scrollback) {
            (Some(status), ..) => (
                format!(" {status} — press any enter to restart "),
                crate::theme::error(),
            ),
            (None, true, _) => (
                " ⟨C-b⟩ - enter to continue, r to refresh ".to_owned(),
                crate::theme::warning(),
            ),
            (None, false, 0) => (
                format!(" shell {cols}x{rows} — C-b to unfocused from shell "),
                crate::theme::border(),
            ),
            (None, false, n) => (format!(" scrollback -{n} "), crate::theme::selection()),
        };

        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(style)
            .title(title)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let inner = pane_inner(area);
        let screen = self.parser.screen();

        frame.render_widget(PseudoTerminal::new(screen).block(self.block()), area);

        // Prefer the host terminal's own cursor over a widget-drawn one: it blinks and picks up the
        // user's cursor shape. ratatui hides it unless a position is set each frame.
        let (row, col) = screen.cursor_position();
        let live = self.exited.is_none() && self.scrollback == 0 && !self.unfocused;
        if live && !screen.hide_cursor() && row < inner.height && col < inner.width {
            frame.set_cursor_position((inner.x + col, inner.y + row));
        }
    }
}

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("State")
            .field("size", &self.size)
            .field("unfocused", &self.unfocused)
            .field("scrollback", &self.scrollback)
            .field("exited", &self.exited)
            .finish()
    }
}

/// The targets screen.
pub struct TerminalScreen {
    context: Context,
    pty_queue: Arc<Mutex<VecDeque<Ev>>>,
    state: Option<anyhow::Result<State>>,
    sessions: SessionWatcher,
}

impl Screen for TerminalScreen {
    fn new(context: Context) -> Self {
        Self {
            sessions: SessionWatcher::new(context.clone()),
            context,
            pty_queue: Default::default(),
            state: None,
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        // A shell that is not running cannot own a session, and the watcher's last reading is not
        // worth the columns once the shell it was attributed to is gone.
        let running = matches!(self.state.as_ref(), Some(Ok(state)) if state.exited.is_none());

        let data = self.sessions.data().read().unwrap();
        let sessions = match running {
            true => data.sessions(),
            false => &[],
        };

        let (shell_area, panel_area) = split(area, !sessions.is_empty());

        if let Some(panel_area) = panel_area {
            panel::draw(frame, panel_area, sessions, data.updated_at);
        }

        // The pty is started and resized below, which needs the watcher back.
        drop(data);

        'main: loop {
            macro_rules! tri {
                ($expr:expr) => {
                    if let Err(err) = $expr {
                        self.state = Some(Err(err));

                        continue 'main;
                    }
                };
            }

            match self.state.as_mut() {
                None => {
                    // The shell is sized by the pane's inner rectangle, and a pane too small to
                    // hold anything would mean a degenerate pty and screen.
                    let inner = pane_inner(shell_area);
                    let (rows, cols) = (inner.height.max(1), inner.width.max(1));

                    let (tx, rx) = std::sync::mpsc::channel();
                    self.state = Some(State::new(rows, cols, tx));

                    // The panel attributes sessions to this shell's process tree, so it can only
                    // start looking once there is a shell to attribute them to.
                    self.sessions.set_shell_pid(
                        self.state
                            .as_ref()
                            .and_then(|state| state.as_ref().ok())
                            .and_then(|state| state.pty.process_id()),
                    );

                    let cx = self.context.clone();
                    let pty_queue = self.pty_queue.clone();

                    std::thread::spawn(move || {
                        while let Ok(ev) = rx.recv() {
                            drain_into(&mut pty_queue.lock().expect("pty_queue poisoned"), ev, &rx);

                            cx.redraw.notify_one();
                        }
                    });
                }
                Some(Ok(state)) => {
                    tri!(state.reconcile_size(shell_area));

                    while let Some(event) = self
                        .pty_queue
                        .lock()
                        .expect("pty_queue poisoned")
                        .pop_front()
                    {
                        tri!(state.handle(event))
                    }

                    state.draw(frame, shell_area);

                    break;
                }
                Some(Err(error)) => {
                    frame.render_widget(
                        Paragraph::new(Span::styled(error.to_string(), Style::default().red()))
                            .centered(),
                        centered(shell_area, Constraint::Fill(1), Constraint::Length(1)),
                    );

                    break;
                }
            }
        }
    }

    fn handle_event(&mut self, event: Event) -> ControlFlow<(), Event> {
        let Some(Ok(state)) = self.state.as_mut() else {
            return ControlFlow::Continue(event);
        };

        match event {
            Event::Key(key) => {
                if state.exited.is_some() {
                    if key.code == KeyCode::Enter {
                        self.state = None;
                        self.sessions.set_shell_pid(None);

                        return ControlFlow::Break(());
                    } else {
                        return ControlFlow::Continue(event);
                    }
                }

                // The panel polls on its own, but a session started a moment ago is worth not
                // waiting for. Handled here rather than in `on_prefix_command` because the
                // watcher belongs to the screen and not to the shell's state.
                if state.unfocused && key.code == KeyCode::Char('r') {
                    self.sessions.refresh();

                    return ControlFlow::Break(());
                }

                match state.on_key(key) {
                    Ok(result) => result.map_continue(Event::Key),
                    Err(err) => {
                        self.state = Some(Err(err));
                        ControlFlow::Break(())
                    }
                }
            }
            Event::Paste(text) if state.exited.is_none() => {
                state.scroll_to_live();
                let bracketed = state.parser.screen().bracketed_paste();

                if let Err(err) = state.pty.write(&encode_paste(&text, bracketed)) {
                    self.state = Some(Err(err));

                    return ControlFlow::Break(());
                }

                ControlFlow::Break(())
            }
            _ => ControlFlow::Continue(event),
        }
    }
}

/// Splits the screen into the shell pane and the session panel beside it.
///
/// The panel has a fixed width and the shell takes the rest, so widening the window grows the
/// shell rather than the panel. The shell gets the whole screen when there is nothing to put in
/// the panel, and also when the panel would leave it less than [`MIN_SHELL_WIDTH`] columns.
fn split(area: Rect, panel: bool) -> (Rect, Option<Rect>) {
    if !panel || area.width < MIN_SHELL_WIDTH + PANEL_WIDTH {
        return (area, None);
    }

    let [shell, panel] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(PANEL_WIDTH)]).areas(area);

    (shell, Some(panel))
}

/// Moves `first`, and everything already waiting behind it, from `rx` into `queue`.
///
/// Taking the whole burst in one go is what keeps a chatty shell to a single redraw instead of one
/// per read. Every event taken off the channel has to be queued: `try_recv` removes it either way,
/// so anything left out here never reaches the emulator and its output is simply lost.
fn drain_into(queue: &mut VecDeque<Ev>, first: Ev, rx: &std::sync::mpsc::Receiver<Ev>) {
    queue.push_back(first);

    while let Ok(event) = rx.try_recv() {
        queue.push_back(event);
    }
}

fn pane_inner(area: Rect) -> Rect {
    Block::bordered().inner(area)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Coalescing a burst into one redraw must not cost any of the output in it — a shell writes
    /// its prompt in several small writes, and dropping one of them leaves the pane blank until
    /// something else is printed.
    #[test]
    fn a_burst_of_output_is_queued_whole() {
        let (tx, rx) = std::sync::mpsc::channel();
        for chunk in ["first", "second", "third"] {
            tx.send(Ev::Pty(chunk.as_bytes().to_vec())).unwrap();
        }

        let mut queue = VecDeque::new();
        let first = rx.recv().unwrap();
        drain_into(&mut queue, first, &rx);

        let queued = queue
            .iter()
            .map(|event| match event {
                Ev::Pty(bytes) => String::from_utf8_lossy(bytes).into_owned(),
                Ev::PtyClosed => "closed".to_owned(),
            })
            .collect::<Vec<_>>();

        assert_eq!(
            queued,
            ["first", "second", "third"],
            "every chunk taken off the channel has to reach the emulator, in order",
        );
    }

    /// Nothing to list means nothing to give columns to, whatever the window size.
    #[test]
    fn without_sessions_the_shell_gets_the_whole_screen() {
        let area = Rect::new(0, 1, 200, 28);

        assert_eq!(split(area, false), (area, None));
    }

    /// The panel keeps its width and the shell takes the rest, so that widening the window grows
    /// the part the user works in.
    #[test]
    fn the_shell_takes_every_column_the_panel_does_not() {
        let (shell, panel) = split(Rect::new(0, 1, 100, 28), true);

        assert_eq!(shell, Rect::new(0, 1, 100 - PANEL_WIDTH, 28));
        assert_eq!(
            panel,
            Some(Rect::new(100 - PANEL_WIDTH, 1, PANEL_WIDTH, 28))
        );
    }

    #[test]
    fn the_panel_is_dropped_before_the_shell_gets_too_narrow_to_work_in() {
        let widest_without_panel = MIN_SHELL_WIDTH + PANEL_WIDTH - 1;

        let (shell, panel) = split(Rect::new(0, 0, widest_without_panel, 10), true);
        assert_eq!(panel, None, "the panel should be gone at this width");
        assert_eq!(
            shell.width, widest_without_panel,
            "the shell should take over"
        );

        let (shell, panel) = split(Rect::new(0, 0, widest_without_panel + 1, 10), true);
        assert_eq!(
            panel.map(|panel| panel.width),
            Some(PANEL_WIDTH),
            "one more column and the panel fits",
        );
        assert_eq!(shell.width, MIN_SHELL_WIDTH);
    }

    /// A window too narrow for anything must still leave the shell a pane rather than a panel.
    #[test]
    fn a_tiny_window_is_all_shell() {
        let (shell, panel) = split(Rect::new(0, 0, 1, 1), true);

        assert_eq!(shell, Rect::new(0, 0, 1, 1));
        assert_eq!(panel, None);
    }
}
