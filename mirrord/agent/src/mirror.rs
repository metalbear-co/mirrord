use axum::async_trait;
use mirrord_protocol::{tcp::LayerTcp, DaemonMessage};

use crate::AgentError;

mod handle_wrapper;
mod sniffer_wrapper;

pub use handle_wrapper::MirrorHandleWrapper;
pub use sniffer_wrapper::SnifferApiWrapper;

#[async_trait]
pub(crate) trait MirrorApi {
    async fn recv(&mut self) -> Option<Result<DaemonMessage, AgentError>>;

    async fn handle_client_message(&mut self, message: LayerTcp) -> Result<(), AgentError>;
}
