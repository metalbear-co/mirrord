//! Terminal UI for mirrord.
//!
//! [`run`] takes over the terminal and returns once the user quits. It is what both the standalone
//! `mirrord-tui` binary and the CLI's `mirrord tui` subcommand call, so everything it touches is
//! restored on the way out - the interface may be one command in a longer-lived process, not the
//! only thing that process ever does.

use std::{io::stdout, sync::Arc};

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
mod telemetry;
mod theme;
mod widgets;

use app::App;
pub use telemetry::Session as TelemetrySession;

/// Runs the interface until the user quits.
///
/// Deliberately does *not* install a tracing subscriber: the caller owns global process state, and
/// a second `init` would panic against the one the CLI has already set up. The standalone binary
/// sets its own up before calling this.
pub async fn run(telemetry: Option<TelemetrySession>) -> anyhow::Result<()> {
    let telemetry = telemetry::Telemetry::new(telemetry);
    telemetry.started();

    // The enhancement flags make terminals speaking the kitty keyboard
    // protocol forward modifier detail - notably Cmd on macOS, which
    // legacy encodings cannot express. Terminals that don't know the
    // protocol ignore the sequence.
    //
    // Best-effort, like their counterparts on the way out: these are enhancements, and failing to
    // enable one is not worth returning through - an early return here would hand the caller back
    // a terminal, a stderr and a panic hook that are still the interface's.
    let _ = execute!(
        stdout(),
        EnableBracketedPaste,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );

    // Auth exec plugins are spawned with our stderr, and they write to it. Nothing may reach the
    // terminal while the interface owns it, so fd 2 goes to the log until we hand the terminal
    // back.
    stderr::capture();

    // `ratatui::init` and the hook after it both chain onto whatever is installed right now, so the
    // caller's hook is held behind an `Arc`: the chain can still reach it while the interface is
    // up, and it can be reinstated on its own once the interface is down. A bare `take_hook` at the
    // end would instead leave the caller running the *default* hook for the rest of the process,
    // losing whatever panic reporting it had set up.
    let caller_hook = Arc::new(std::panic::take_hook());
    let chained = Arc::clone(&caller_hook);
    std::panic::set_hook(Box::new(move |info| chained(info)));

    let mut app = App::new(telemetry.clone());
    let mut terminal = ratatui::init();
    // Best-effort for the same reason as above, and more so here: by this point stderr is captured,
    // the panic hook is replaced and the alternate screen is up, so returning through `?` would
    // hand all three back to the caller still belonging to the interface.
    let _ = execute!(stdout(), EnableBracketedPaste);
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

    // Only reached when the interface is quit rather than killed, which is why the tab counts it
    // carries are the one part of this that is allowed to go missing.
    telemetry.closed();

    let _ = execute!(stdout(), DisableBracketedPaste);
    let _ = execute!(stdout(), PopKeyboardEnhancementFlags);

    ratatui::restore();
    // Past this point stderr is the terminal's again, so a returned error still prints.
    stderr::restore();
    // And the caller gets its own hook back, rather than one that would restore a terminal the
    // interface has already handed over.
    std::panic::set_hook(Box::new(move |info| caller_hook(info)));

    result
}
