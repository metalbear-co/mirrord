//! This module contains [`TcpMirrorApi`] and its implementations:
//!
//! * [`MirrorHandleWrapper`] - using [`crate::incoming::MirrorHandle`] for mirroring with iptables
//!   redirections.
//! * [`SnifferApiWrapper`] - using [`crate::sniffer::api::TcpSnifferApi`] for mirroring with a raw
//!   socket.

use axum::async_trait;
use mirrord_protocol::{tcp::LayerTcp, DaemonMessage};

use crate::AgentError;

mod handle_wrapper;
mod sniffer_wrapper;

pub use handle_wrapper::MirrorHandleWrapper;
pub use sniffer_wrapper::SnifferApiWrapper;

#[async_trait] // allows for dynamic dispatch
pub(crate) trait TcpMirrorApi: 'static + Send + Sync {
    /// Returns the next message from the mirroring task.
    ///
    /// If there's nothing to report (no active port subscriptions),
    /// implementors can return [`None`] or never resolve.
    async fn recv(&mut self) -> Option<Result<DaemonMessage, AgentError>>;

    /// Processes the given [`LayerTcp`] message from the client.
    async fn handle_client_message(&mut self, message: LayerTcp) -> Result<(), AgentError>;
}
