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
use tracing::{debug, error};

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
            message = remote_stream.next() => {
                match message {
                    Some(data) => {
                        match local_stream.write_all(&data).await {
                            Ok(_) => {},
                            Err(err) => {error!("writing to local stream err {err:?}"); break;}
                        }
                    },
                    None => {
                        debug!("tcp tunnel exiting due to remote stream closed");
                        break;
                    }
                }
            },
            // Read the application's response from the socket and discard the data, so that the socket doesn't fill up.
            res = local_stream.read(&mut buffer) => {
                match res {
                    Err(ref err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        continue;
                        },
                    Err(err) => {
                        error!("local stream received an error reading {err:?}");
                        break;
                    }
                    Ok(n) if n == 0 => {
                        debug!("tcp tunnel exiting due to local stream closed");
                        break;
                    },
                    Ok(_) => {}
                }

            }
        }
    }
    debug!("exiting tcp tunnel");
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
                Some(message) => self.handle_incoming_message(message).await?,
                None => break,
            }
        }
        Ok(())
    }
}

#[async_trait]
impl TCPHandler for TCPMirrorHandler {
    /// Handle NewConnection messages
    async fn handle_new_connection(&mut self, conn: NewTCPConnection) -> Result<(), LayerError> {
        let stream = self.create_local_stream(&conn).await?;
        // .ok_or_else(|| LayerError::StreamFailed)?;

        let (sender, receiver) = channel::<Vec<u8>>(1000);

        let conn = Connection::new(conn.connection_id, sender);
        self.connections.insert(conn);

        task::spawn(async move { tcp_tunnel(stream, receiver).await });

        Ok(())
    }

    /// Handle New Data messages
    async fn handle_new_data(&mut self, data: TCPData) -> Result<(), LayerError> {
        let mut connection = self
            .connections
            .take(&data.connection_id)
            .ok_or_else(|| LayerError::NoConnection)?;

        connection.write(data.bytes).await?;
        self.connections.insert(connection);

        Ok(())
    }

    /// Handle connection close
    fn handle_close(&mut self, close: TCPClose) -> Result<(), LayerError> {
        let connection_id = close.connection_id;

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
