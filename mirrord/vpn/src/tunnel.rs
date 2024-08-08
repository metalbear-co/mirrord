use std::io;

use futures::{Sink, SinkExt, Stream, StreamExt};
use mirrord_protocol::{
    vpn::{ClientVpn, ServerVpn},
    ClientMessage, DaemonMessage,
};
use tokio::sync::mpsc;

pub struct VpnTunnel<S> {
    agent_tx: mpsc::Sender<ClientMessage>,
    agent_rx: mpsc::Receiver<DaemonMessage>,
    stream: S,
}

impl<S> VpnTunnel<S>
where
    S: Stream<Item = io::Result<Vec<u8>>> + Sink<Vec<u8>, Error = io::Error>,
{
    pub fn new(
        agent_tx: mpsc::Sender<ClientMessage>,
        agent_rx: mpsc::Receiver<DaemonMessage>,
        stream: S,
    ) -> Self {
        VpnTunnel {
            agent_tx,
            agent_rx,
            stream,
        }
    }

    pub async fn start(self) -> Result<(), mpsc::error::SendError<ClientMessage>> {
        let VpnTunnel {
            mut agent_rx,
            agent_tx,
            stream,
        } = self;

        tokio::pin!(stream);

        agent_tx
            .send(ClientMessage::Vpn(ClientVpn::OpenSocket))
            .await?;

        'main: loop {
            tokio::select! {
                packet = stream.next() => {
                    let packet = packet.unwrap().unwrap();
                    agent_tx
                        .send(mirrord_protocol::ClientMessage::Vpn(ClientVpn::Packet(
                            packet,
                        )))
                        .await?
                }
                message = agent_rx.recv() => {
                    match message {
                        None => break 'main,
                        Some(DaemonMessage::Vpn(ServerVpn::Packet(packet))) => {
                            if let Err(err) = stream.send(packet).await {
                                tracing::warn!(%err, "Unable to pipe back packet")
                            }
                        }
                        _ => unimplemented!("Unexpected response from agent"),
                    }
                }
            }
        }

        Ok(())
    }
}
