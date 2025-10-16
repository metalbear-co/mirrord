use std::{
    pin::Pin,
    sync::LazyLock,
    task::{Context, Poll, ready},
};

use futures::{Stream, StreamExt};
use mirrord_protocol::{
    ClientMessage, DaemonMessage, LogLevel,
    vpn::{ClientVpn, NetworkConfiguration, ServerVpn},
};
use mirrord_protocol_io::{Client, Connection};
use semver::VersionReq;
use tokio::sync::oneshot;

use crate::error::VpnError;

pub static MINIMAL_PROTOCOL_VERSION: LazyLock<VersionReq> = LazyLock::new(|| {
    ">=1.10.0"
        .parse()
        .expect("MINIMAL_PROTOCOL_VERSION should be valid")
});

pub struct VpnAgent {
    connection: Connection<Client>,
    pong: Option<oneshot::Sender<()>>,
}

impl VpnAgent {
    pub async fn try_create(connection: Connection<Client>) -> Result<Self, VpnError> {
        let mut vpn_agnet = VpnAgent::new(connection);

        let Some(agent_protocol_version) = vpn_agnet
            .send_and_get_response(
                ClientMessage::SwitchProtocolVersion(mirrord_protocol::VERSION.clone()),
                |message| match message {
                    DaemonMessage::SwitchProtocolVersionResponse(response) => Some(response),
                    _ => None,
                },
            )
            .await?
        else {
            return Err(VpnError::AgentUnexpectedResponse);
        };

        if !MINIMAL_PROTOCOL_VERSION.matches(&agent_protocol_version) {
            return Err(VpnError::AgentProtocolVersionMismatch(
                agent_protocol_version,
            ));
        }

        Ok(vpn_agnet)
    }

    fn new(connection: Connection<Client>) -> Self {
        VpnAgent {
            connection,
            pong: None,
        }
    }

    pub async fn ping(&mut self) -> Result<oneshot::Receiver<()>, VpnError> {
        let (tx, rx) = oneshot::channel();
        self.pong = Some(tx);

        self.send(ClientMessage::Ping).await;

        Ok(rx)
    }

    pub async fn get_network_configuration(&mut self) -> Result<NetworkConfiguration, VpnError> {
        let response = self
            .send_and_get_response(
                ClientMessage::Vpn(ClientVpn::GetNetworkConfiguration),
                |message| match message {
                    DaemonMessage::Vpn(response) => Some(response),
                    _ => None,
                },
            )
            .await?;

        match response {
            Some(ServerVpn::NetworkConfiguration(network)) => Ok(network),
            _ => Err(VpnError::AgentUnexpectedResponse),
        }
    }

    pub async fn open_socket(&self) {
        self.send(ClientMessage::Vpn(ClientVpn::OpenSocket)).await
    }

    pub async fn send_packet(&self, packet: Vec<u8>) {
        self.send(ClientMessage::Vpn(ClientVpn::Packet(packet.into())))
            .await
    }

    pub async fn send(&self, request: ClientMessage) {
        self.connection.send(request).await
    }

    pub async fn send_and_get_response<T>(
        &mut self,
        request: ClientMessage,
        response_filter: impl Fn(DaemonMessage) -> Option<T>,
    ) -> Result<Option<T>, VpnError> {
        self.send(request).await;

        self.next()
            .await
            .map(response_filter)
            .ok_or_else(|| VpnError::AgentNoResponse)
    }
}

impl Stream for VpnAgent {
    type Item = DaemonMessage;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let result = ready!(self.connection.rx.poll_recv(cx));

        match result {
            Some(DaemonMessage::LogMessage(message)) => {
                match message.level {
                    LogLevel::Error => {
                        tracing::error!(message = %message.message, "agent sent error message")
                    }
                    LogLevel::Warn => {
                        tracing::warn!(message = %message.message, "agent sent warn message")
                    }
                    LogLevel::Info => {
                        tracing::info!(message = %message.message, "agent sent info message")
                    }
                }

                self.poll_next(cx)
            }
            Some(DaemonMessage::Pong) if self.pong.is_some() => {
                let _ = self
                    .as_mut()
                    .pong
                    .take()
                    .expect("pong should contain sender")
                    .send(());

                self.poll_next(cx)
            }
            _ => Poll::Ready(result),
        }
    }
}
