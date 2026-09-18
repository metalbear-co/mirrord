//! Unix signal handling implementation using nix and tokio signal handlers.

use std::io;

use nix::sys::signal::Signal;
#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};

const SIGNAL_EXIT_CODE_OFFSET: i32 = 128;

#[derive(Clone, Copy, Debug)]
pub enum ShutdownSignal {
    Interrupt,
    Terminate,
    Hangup,
}

impl ShutdownSignal {
    pub fn unix_signal(self) -> Signal {
        match self {
            Self::Interrupt => Signal::SIGINT,
            Self::Terminate => Signal::SIGTERM,
            Self::Hangup => Signal::SIGHUP,
        }
    }

    pub fn forced_exit_code(self) -> i32 {
        // Shells conventionally report signal termination as 128 plus the
        // signal number exposed by `nix`.
        SIGNAL_EXIT_CODE_OFFSET + self.unix_signal() as i32
    }
}

/// Platform signal listeners installed before any service process is spawned.
///
/// tokio exposes one stream type per signal source rather than a combined
/// stream. Keeping every listener alive in this struct preserves all handlers,
/// while [`receive_signal`] selects the first source that produces an event.
pub struct SignalStreams {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
}

pub fn signal_streams() -> io::Result<SignalStreams> {
    Ok(SignalStreams {
        interrupt: signal(SignalKind::interrupt())?,
        terminate: signal(SignalKind::terminate())?,
        hangup: signal(SignalKind::hangup())?,
    })
}

pub async fn receive_signal(signals: &mut SignalStreams) -> io::Result<ShutdownSignal> {
    tokio::select! {
        received = signals.interrupt.recv() => received.map(|_| ShutdownSignal::Interrupt),
        received = signals.terminate.recv() => received.map(|_| ShutdownSignal::Terminate),
        received = signals.hangup.recv() => received.map(|_| ShutdownSignal::Hangup),
    }
    .ok_or_else(|| io::Error::other("shutdown signal stream closed"))
}
