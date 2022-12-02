use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
};

use futures::StreamExt;
use mirrord_protocol::{
    tcp::{DaemonTcp, LayerTcpSteal, NewTcpConnection, TcpClose, TcpData},
    ConnectionId, Port,
};
use streammap_ext::StreamMap;
use tokio::{
    io::{AsyncWriteExt, ReadHalf, WriteHalf},
    net::{TcpListener, TcpStream},
    select,
    sync::mpsc::{Receiver, Sender},
};
use tokio_util::io::ReaderStream;
use tracing::{debug, error, info, log::warn};

use super::*;
use crate::error::{AgentError, Result};

pub(super) struct StealWorker {
    pub(super) sender: Sender<DaemonTcp>,
    iptables: SafeIpTables<iptables::IPTables>,
    ports: HashSet<Port>,
    listen_port: Port,
    write_streams: HashMap<ConnectionId, WriteHalf<TcpStream>>,
    read_streams: StreamMap<ConnectionId, ReaderStream<ReadHalf<TcpStream>>>,
    connection_index: u64,
}

impl StealWorker {
    #[tracing::instrument(level = "trace", skip(sender))]
    pub(super) fn new(sender: Sender<DaemonTcp>, listen_port: Port) -> Result<Self> {
        Ok(Self {
            sender,
            iptables: SafeIpTables::new(iptables::new(false).unwrap())?,
            ports: HashSet::default(),
            listen_port,
            write_streams: HashMap::default(),
            read_streams: StreamMap::default(),
            connection_index: 0,
        })
    }

    #[tracing::instrument(level = "trace", skip(self, rx, listener))]
    pub(super) async fn start(
        mut self,
        mut rx: Receiver<LayerTcpSteal>,
        listener: TcpListener,
    ) -> Result<()> {
        loop {
            select! {
                msg = rx.recv() => {
                    if let Some(msg) = msg {
                        self.handle_client_message(msg).await?;
                    } else {
                        debug!("rx closed, breaking");
                        break;
                    }
                },
                accept = listener.accept() => {
                    match accept {
                        Ok((stream, address)) => {
                            self.handle_incoming_connection(stream, address).await?;
                        },
                        Err(err) => {
                            error!("accept error {err:?}");
                            break;
                        }
                    }
                },
                message = self.next() => {
                    if let Some(message) = message {
                        self.sender.send(message).await?;
                    }
                }
            }
        }
        debug!("TCP Stealer exiting");
        Ok(())
    }

    #[tracing::instrument(level = "trace", skip(self))]
    async fn handle_client_message(&mut self, message: LayerTcpSteal) -> Result<()> {
        use LayerTcpSteal::*;
        match message {
            PortSubscribe(port) => {
                if self.ports.contains(&port) {
                    warn!("Port {port:?} is already subscribed");
                    Ok(())
                } else {
                    debug!("adding redirect rule");
                    self.iptables.add_redirect(port, self.listen_port)?;
                    self.ports.insert(port);
                    self.sender.send(DaemonTcp::Subscribed).await?;
                    debug!("sent subscribed");
                    Ok(())
                }
            }
            ConnectionUnsubscribe(connection_id) => {
                info!("Closing connection {connection_id:?}");
                self.write_streams.remove(&connection_id);
                self.read_streams.remove(&connection_id);
                Ok(())
            }
            PortUnsubscribe(port) => {
                if self.ports.remove(&port) {
                    self.iptables.remove_redirect(port, self.listen_port)
                } else {
                    warn!("removing unsubscribed port {port:?}");
                    Ok(())
                }
            }

            Data(data) => {
                if let Some(stream) = self.write_streams.get_mut(&data.connection_id) {
                    stream.write_all(&data.bytes[..]).await?;
                    Ok(())
                } else {
                    warn!(
                        "Trying to send data to closed connection {:?}",
                        data.connection_id
                    );
                    Ok(())
                }
            }
        }
    }

    #[tracing::instrument(level = "trace", skip(self, stream))]
    async fn handle_incoming_connection(
        &mut self,
        stream: TcpStream,
        address: SocketAddr,
    ) -> Result<()> {
        let real_addr = orig_dst::orig_dst_addr(&stream)?;
        if !self.ports.contains(&real_addr.port()) {
            return Err(AgentError::UnexpectedConnection(real_addr.port()));
        }
        let connection_id = self.connection_index;
        self.connection_index += 1;

        let (read_half, write_half) = tokio::io::split(stream);
        self.write_streams.insert(connection_id, write_half);
        self.read_streams
            .insert(connection_id, ReaderStream::new(read_half));

        let new_connection = DaemonTcp::NewConnection(NewTcpConnection {
            connection_id,
            destination_port: real_addr.port(),
            source_port: address.port(),
            address: address.ip(),
        });
        self.sender.send(new_connection).await?;
        debug!("sent new connection");
        Ok(())
    }

    async fn next(&mut self) -> Option<DaemonTcp> {
        let (connection_id, value) = self.read_streams.next().await?;
        match value {
            Some(Ok(bytes)) => Some(DaemonTcp::Data(TcpData {
                connection_id,
                bytes: bytes.to_vec(),
            })),
            Some(Err(err)) => {
                error!("connection id {connection_id:?} read error: {err:?}");
                None
            }
            None => Some(DaemonTcp::Close(TcpClose { connection_id })),
        }
    }
}
