//! Windows console event handling implementation using tokio signal handlers.

use std::io;

// Windows control-event constants identify the event rather than prescribe an
// exit status. Keep the shell-style interrupted status used by the CLI instead
// of switching to the unrelated native `STATUS_CONTROL_C_EXIT` value.
const INTERRUPTED_EXIT_CODE: i32 = 130;

#[derive(Clone, Copy, Debug)]
pub enum ShutdownSignal {
    CtrlC,
    CtrlBreak,
}

impl ShutdownSignal {
    pub fn forced_exit_code(self) -> i32 {
        match self {
            Self::CtrlC | Self::CtrlBreak => INTERRUPTED_EXIT_CODE,
        }
    }
}

/// Windows console-event listeners installed before any service is spawned.
///
/// Ctrl-C and Ctrl-Break use distinct tokio listener types, so both must remain
/// alive while [`receive_signal`] waits for whichever event arrives first.
pub struct SignalStreams {
    ctrl_c: tokio::signal::windows::CtrlC,
    ctrl_break: tokio::signal::windows::CtrlBreak,
}

pub fn signal_streams() -> io::Result<SignalStreams> {
    Ok(SignalStreams {
        ctrl_c: tokio::signal::windows::ctrl_c()?,
        ctrl_break: tokio::signal::windows::ctrl_break()?,
    })
}

pub async fn receive_signal(signals: &mut SignalStreams) -> io::Result<ShutdownSignal> {
    tokio::select! {
        received = signals.ctrl_c.recv() => received.map(|_| ShutdownSignal::CtrlC),
        received = signals.ctrl_break.recv() => received.map(|_| ShutdownSignal::CtrlBreak),
    }
    .ok_or_else(|| io::Error::other("shutdown signal stream closed"))
}
