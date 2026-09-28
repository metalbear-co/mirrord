//! Platform-specific signal handling primitives for Unix and Windows.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub(super) use unix::{ShutdownSignal, SignalStreams, receive_signal, signal_streams};
#[cfg(windows)]
pub(super) use windows::{ShutdownSignal, SignalStreams, receive_signal, signal_streams};
