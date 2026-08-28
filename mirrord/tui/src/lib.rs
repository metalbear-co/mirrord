use std::io::stdout;

use ratatui::crossterm::{
    event::{
        DisableBracketedPaste, EnableBracketedPaste, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
};

mod app;
mod context;
mod helpers;
mod local_sessions;
mod scope;
mod screens;
mod status;
mod stderr;
mod theme;
mod widgets;

use app::App;

pub async fn run() -> anyhow::Result<()> {
    // The enhancement flags make terminals speaking the kitty keyboard
    // protocol forward modifier detail - notably Cmd on macOS, which
    // legacy encodings cannot express. Terminals that don't know the
    // protocol ignore the sequence.
    execute!(
        stdout(),
        EnableBracketedPaste,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )?;

    // Auth exec plugins are spawned with our stderr, and they write to it. Nothing may reach the
    // terminal while the interface owns it, so fd 2 goes to the log until we hand the terminal
    // back.
    stderr::capture();

    let mut app = App::new();
    let mut terminal = ratatui::init();
    execute!(stdout(), EnableBracketedPaste)?;
    // `ratatui::init` installed a hook that restores the terminal; chain onto it so a panic cannot
    // leave bracketed paste enabled in the user's shell either.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(stdout(), DisableBracketedPaste);
        // The panic message is written to stderr, which is currently a pipe into the log.
        stderr::restore();
        hook(info);
    }));

    let result = app.run(&mut terminal).await;

    let _ = execute!(stdout(), DisableBracketedPaste);
    let _ = execute!(stdout(), PopKeyboardEnhancementFlags);

    ratatui::restore();
    // Past this point stderr is the terminal's again, so a returned error still prints.
    stderr::restore();

    result
}
