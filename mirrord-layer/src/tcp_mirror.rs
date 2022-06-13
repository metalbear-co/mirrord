use std::{
    borrow::Borrow,
    collections::HashSet,
    hash::{Hash, Hasher},
};

use anyhow::Result;
use async_trait::async_trait;
use mirrord_protocol::{ConnectionID, NewTCPConnection, TCPClose, TCPData};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    select,
    sync::mpsc::{channel, Receiver, Sender},
    task,
};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tracing::{debug, error, warn};

use crate::{
    common::Listen,
    error::LayerError,
    tcp::{TCPApi, TCPHandler, TrafficHandlerInput},
};

const CHANNEL_SIZE: usize = 1024;

type TrafficHandlerReceiver = Receiver<TrafficHandlerInput>;

async fn tcp_tunnel(mut local_stream: TcpStream, remote_stream: Receiver<Vec<u8>>) {
    let mut remote_stream = ReceiverStream::new(remote_stream);
    let mut buffer = vec![0; 1024];

    loop {
        select! {
            bytes = remote_stream.next() => {
                match bytes {
                    Some(bytes) => {
                        if let Err(fail) = local_stream.write_all(&bytes).await {
                            error!("Failed writing to local_stream with {:#?}!", fail);
                            break;
                        }
                    },
                    None => {
                        warn!("tcp_tunnel -> exiting due to remote stream closed!");
                        break;
                    }
                }
            },
            // Read the application's response from the socket and discard the data, so that the socket doesn't fill up.
            read = local_stream.read(&mut buffer) => {
                match read {
                    Err(fail) if fail.kind() == std::io::ErrorKind::WouldBlock => {
                        continue;
                    },
                    Err(fail) => {
                        error!("Failed reading local_stream with {:#?}", fail);
                        break;
                    }
                    Ok(read_amount) if read_amount == 0 => {
                        warn!("tcp_tunnel -> exiting due to local stream closed!");
                        break;
                    },
                    Ok(_) => {}
                }

            }
        }
    }
    debug!("tcp_tunnel -> exiting");
}

struct Connection {
    writer: Sender<Vec<u8>>,
    id: ConnectionID,
}

impl Eq for Connection {}

impl PartialEq for Connection {
    fn eq(&self, other: &Connection) -> bool {
        self.id == other.id
    }
}

impl Hash for Connection {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl Connection {
    pub fn new(id: ConnectionID, writer: Sender<Vec<u8>>) -> Self {
        Self { id, writer }
    }

    pub async fn write(&mut self, data: Vec<u8>) -> Result<(), LayerError> {
        self.writer.send(data).await.map_err(From::from)
    }
}

impl Borrow<ConnectionID> for Connection {
    fn borrow(&self) -> &ConnectionID {
        &self.id
    }
}

/// Handles traffic mirroring
#[derive(Default)]
pub struct TCPMirrorHandler {
    ports: HashSet<Listen>,
    connections: HashSet<Connection>,
}

impl TCPMirrorHandler {
    pub async fn run(mut self, mut incoming: TrafficHandlerReceiver) -> Result<(), LayerError> {
        loop {
            match incoming.recv().await {
                Some(message) => {
                    let _ = self
                        .handle_incoming_message(message)
                        .await
                        .inspect_err(|fail| {
                            error!("TCPMirrorHandler::run failed with {:#?}", fail)
                        });
                }
                None => break,
            }
        }
        Ok(())
    }
}

#[async_trait]
impl TCPHandler for TCPMirrorHandler {
    /// Handle NewConnection messages
    async fn handle_new_connection(
        &mut self,
        tcp_connection: NewTCPConnection,
    ) -> Result<(), LayerError> {
        debug!("handle_new_connection -> {:#?}", tcp_connection);

        let stream = self.create_local_stream(&tcp_connection).await?;

        let (sender, receiver) = channel::<Vec<u8>>(1000);

        let new_connection = Connection::new(tcp_connection.connection_id, sender);
        self.connections.insert(new_connection);

        task::spawn(async move { tcp_tunnel(stream, receiver).await });

        Ok(())
    }

    /// Handle New Data messages
    async fn handle_new_data(&mut self, data: TCPData) -> Result<(), LayerError> {
        debug!("handle_new_data -> id {:#?}", data.connection_id);

        // TODO: "remove -> op -> insert" pattern here, maybe we could improve the overlying
        // abstraction to use something that has mutable access.
        let mut connection = self
            .connections
            .take(&data.connection_id)
            .ok_or_else(|| LayerError::NoConnectionId(data.connection_id))?;

        debug!(
            "handle_new_data -> writing {:#?} bytes to id {:#?}",
            data.bytes.len(),
            connection.id
        );
        // TODO: Due to the above, if we fail here this connection is leaked (-agent won't be told
        // that we just removed it).
        connection.write(data.bytes).await?;

        self.connections.insert(connection);
        debug!("handle_new_data -> success");

        Ok(())
    }

    /// Handle connection close
    fn handle_close(&mut self, close: TCPClose) -> Result<(), LayerError> {
        debug!("handle_close -> close {:#?}", close);

        let TCPClose { connection_id } = close;

        // Dropping the connection -> Sender drops -> Receiver disconnects -> tcp_tunnel ends
        self.connections
            .remove(&connection_id)
            .then_some(())
            .ok_or(LayerError::ConnectionIdNotFound(connection_id))
    }

    fn ports(&self) -> &HashSet<Listen> {
        &self.ports
    }

    fn ports_mut(&mut self) -> &mut HashSet<Listen> {
        &mut self.ports
    }
}

unsafe impl Send for TCPMirrorHandler {}

pub fn create_tcp_mirror_handler() -> (TCPMirrorHandler, TCPApi, TrafficHandlerReceiver)
where
{
    let (traffic_in_tx, traffic_in_rx) = channel(CHANNEL_SIZE);
    let handler = TCPMirrorHandler::default();
    let control = TCPApi::new(traffic_in_tx);
    (handler, control, traffic_in_rx)
}
