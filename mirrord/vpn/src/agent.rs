use std::{
    pin::Pin,
    task::{ready, Context, Poll},
};

use futures::Stream;
use mirrord_protocol::{vpn::ClientVpn, ClientMessage, DaemonMessage, LogLevel};
use tokio::sync::{mpsc, oneshot};

pub struct VpnAgent {
    tx: mpsc::Sender<ClientMessage>,
    rx: mpsc::Receiver<DaemonMessage>,

    pong: Option<oneshot::Sender<()>>,
}

impl VpnAgent {
    pub fn new(tx: mpsc::Sender<ClientMessage>, rx: mpsc::Receiver<DaemonMessage>) -> Self {
        VpnAgent { tx, rx, pong: None }
    }

    pub async fn ping(
        &mut self,
    ) -> Result<oneshot::Receiver<()>, mpsc::error::SendError<ClientMessage>> {
        let (tx, rx) = oneshot::channel();
        self.pong = Some(tx);

        self.tx.send(ClientMessage::Ping).await?;

        Ok(rx)
    }

    pub async fn open_socket(&self) -> Result<(), mpsc::error::SendError<ClientMessage>> {
        self.tx
            .send(ClientMessage::Vpn(ClientVpn::OpenSocket))
            .await
    }

    pub async fn send_packet(
        &self,
        packet: Vec<u8>,
    ) -> Result<(), mpsc::error::SendError<ClientMessage>> {
        self.tx
            .send(ClientMessage::Vpn(ClientVpn::Packet(packet)))
            .await
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
