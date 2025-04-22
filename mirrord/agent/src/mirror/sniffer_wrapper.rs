use axum::async_trait;
use mirrord_protocol::{tcp::LayerTcp, DaemonMessage};

use super::TcpMirrorApi;
use crate::{sniffer::api::TcpSnifferApi, AgentError};

pub struct SnifferApiWrapper {
    api: TcpSnifferApi,
    ready_message: Option<DaemonMessage>,
}

impl SnifferApiWrapper {
    pub fn new(api: TcpSnifferApi) -> Self {
        Self {
            api,
            ready_message: None,
        }
    }
}

#[async_trait]
impl TcpMirrorApi for SnifferApiWrapper {
    async fn recv(&mut self) -> Option<Result<DaemonMessage, AgentError>> {
        if let Some(message) = self.ready_message.take() {
            return Some(Ok(message));
        }

        let (message, log) = match self.api.recv().await {
            Ok((message, log)) => (message, log),
            Err(error) => return Some(Err(error)),
        };

        if let Some(log) = log {
            self.ready_message.replace(DaemonMessage::Tcp(message));
            Some(Ok(DaemonMessage::LogMessage(log)))
        } else {
            Some(Ok(DaemonMessage::Tcp(message)))
        }
    }

    async fn handle_client_message(&mut self, message: LayerTcp) -> Result<(), AgentError> {
        self.api.handle_client_message(message).await
    }
}
