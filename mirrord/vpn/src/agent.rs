use std::{
    pin::Pin,
    sync::LazyLock,
    task::{ready, Context, Poll},
};

use futures::{Stream, StreamExt};
use mirrord_protocol::{
    vpn::{ClientVpn, NetworkConfiguration, ServerVpn},
    ClientMessage, DaemonMessage, LogLevel,
};
use semver::VersionReq;
use tokio::sync::{mpsc, oneshot};

use crate::error::VpnError;

pub static MINIMAL_PROTOCOL_VERSION: LazyLock<VersionReq> = LazyLock::new(|| {
    ">=1.10.0"
        .parse()
        .expect("MINIMAL_PROTOCOL_VERSION should be valid")
});

pub struct VpnAgent {
    tx: mpsc::Sender<ClientMessage>,
    rx: mpsc::Receiver<DaemonMessage>,

    pong: Option<oneshot::Sender<()>>,
}

impl VpnAgent {
    pub async fn try_create(
        tx: mpsc::Sender<ClientMessage>,
        rx: mpsc::Receiver<DaemonMessage>,
    ) -> Result<Self, VpnError> {
        let mut vpn_agnet = VpnAgent::new(tx, rx);

        let Some(agent_protocol_version) = vpn_agnet
            .send_and_get_response(
                ClientMessage::SwitchProtocolVersion(mirrord_protocol::VERSION.clone()),
                |message| match message {
                    DaemonMessage::SwitchProtocolVersionResponse(response) => Some(response),
                    _ => None,
                },
            )
            .await
            .map_err(VpnError::from)?
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

    fn new(tx: mpsc::Sender<ClientMessage>, rx: mpsc::Receiver<DaemonMessage>) -> Self {
        VpnAgent { tx, rx, pong: None }
    }

    pub async fn ping(&mut self) -> Result<oneshot::Receiver<()>, VpnError> {
        let (tx, rx) = oneshot::channel();
        self.pong = Some(tx);

        self.send(ClientMessage::Ping).await?;

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
            .await
            .map_err(VpnError::from)?;

        match response {
            Some(ServerVpn::NetworkConfiguration(network)) => Ok(network),
            _ => Err(VpnError::AgentUnexpectedResponse),
        }
    }

    pub async fn open_socket(&self) -> Result<(), VpnError> {
        self.send(ClientMessage::Vpn(ClientVpn::OpenSocket)).await
    }

    pub async fn send_packet(&self, packet: Vec<u8>) -> Result<(), VpnError> {
        self.send(ClientMessage::Vpn(ClientVpn::Packet(packet)))
            .await
    }

    pub async fn send(&self, request: ClientMessage) -> Result<(), VpnError> {
        self.tx
            .send(request)
            .await
            .map_err(|_| VpnError::ClientMessageDropped)
    }

    pub async fn send_and_get_response<T>(
        &mut self,
        request: ClientMessage,
        response_filter: impl Fn(DaemonMessage) -> Option<T>,
    ) -> Result<Option<T>, VpnError> {
        self.tx
            .send(request)
            .await
            .map_err(|_| VpnError::ClientMessageDropped)?;

        self.next()
            .await
            .map(response_filter)
            .ok_or_else(|| VpnError::AgentNoResponse)
    }
}

impl Stream for VpnAgent {
    type Item = DaemonMessage;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let result = ready!(self.rx.poll_recv(cx));

        match result {
            Some(DaemonMessage::LogMessage(message)) => {
                match message.level {
                    LogLevel::Error => {
                        tracing::error!(message = %message.message, "agent sent error message")
                    }
                    LogLevel::Warn => {
                        tracing::warn!(message = %message.message, "agent sent warn message")
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
