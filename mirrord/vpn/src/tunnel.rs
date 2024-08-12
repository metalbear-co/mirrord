use std::{io, time::Duration};

use futures::{Sink, SinkExt, Stream, StreamExt};
use mirrord_protocol::{vpn::ServerVpn, ClientMessage, DaemonMessage};
use tokio::sync::mpsc;

use crate::agent::VpnAgent;

pub struct VpnTunnel<S> {
    agent: VpnAgent,
    stream: S,
}

impl<S> VpnTunnel<S>
where
    S: Stream<Item = io::Result<Vec<u8>>> + Sink<Vec<u8>, Error = io::Error>,
{
    pub fn new(agent: VpnAgent, stream: S) -> Self {
        VpnTunnel { agent, stream }
    }

    pub async fn start(self) -> Result<(), mpsc::error::SendError<ClientMessage>> {
        let VpnTunnel { mut agent, stream } = self;
        tokio::pin!(stream);

        let mut ping_interval = tokio::time::interval(Duration::from_secs(30));
        let mut pong_timeout = Box::pin(None);

        tokio::pin!(stream);

        agent.open_socket().await?;

        'main: loop {
            tokio::select! {
                packet = stream.next() => {
                    let packet = packet.unwrap().unwrap();
                    agent
                        .send_packet(packet)
                        .await?
                }
                message = agent.next() => {
                    match message {
                        None => break 'main,
                        Some(DaemonMessage::Vpn(ServerVpn::Packet(packet))) => {
                            if let Err(error) = stream.send(packet).await {
                                tracing::warn!(%error, "unable to pipe back packet")
                            }
                        }
                        _ => unimplemented!("Unexpected response from agent"),
                    }
                }
                _ = ping_interval.tick() => {
                    let pong = agent.ping().await?;
                    pong_timeout = Box::pin(Some(tokio::time::timeout(Duration::from_secs(10), pong)));
                }
                pong = async { pong_timeout.as_mut().as_pin_mut().expect("pong_timeout should contain timeout").await }, if pong_timeout.is_some() => {
                    match pong {
                        Err(error) => {
                            tracing::error!(%error, "timeout wating for pong from agent");
                            break 'main
                        },
                        Ok(Err(error)) => {
                            tracing::error!(%error, "Unable to pipe back packet");
                            break 'main
                        },
                        _ => {
                            pong_timeout = Box::pin(None);
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
