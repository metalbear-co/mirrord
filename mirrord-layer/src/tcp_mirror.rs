use std::{
    borrow::Borrow,
    collections::HashSet,
    hash::{Hash, Hasher},
    time::Duration,
};

use async_trait::async_trait;
use mirrord_protocol::{
    tcp::{NewTcpConnection, TcpClose, TcpData},
    ConnectionID,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    select,
    sync::mpsc::{channel, Receiver, Sender},
    task,
    time::sleep,
};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tracing::{debug, error, warn};

use crate::{
    error::{LayerError, Result},
    tcp::{Listen, TcpHandler},
};

async fn tcp_tunnel(mut local_stream: TcpStream, remote_stream: Receiver<Vec<u8>>) {
    let mut remote_stream = ReceiverStream::new(remote_stream);
    let mut buffer = vec![0; 1024];
    let mut remote_stream_closed = false;
    loop {
        select! {
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
            },
            bytes = remote_stream.next(), if !remote_stream_closed => {
                match bytes {
                    Some(bytes) => {
                        if let Err(fail) = local_stream.write_all(&bytes).await {
                            error!("Failed writing to local_stream with {:#?}!", fail);
                            break;
                        }
                    },
                    None => {
                        // The remote stream has closed, sleep 1 second to let the local stream drain (i.e if a response is being sent)
                        debug!("remote stream closed");
                        remote_stream_closed = true;

                    }
                }
            },
            _ = sleep(Duration::from_secs(1)), if remote_stream_closed => {
                warn!("tcp_tunnel -> exiting due to remote stream closed!");
                break;
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

    pub async fn write(&mut self, data: Vec<u8>) -> Result<()> {
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
pub struct TcpMirrorHandler {
    ports: HashSet<Listen>,
    connections: HashSet<Connection>,
}

#[async_trait]
impl TcpHandler for TcpMirrorHandler {
    /// Handle NewConnection messages
    async fn handle_new_connection(&mut self, tcp_connection: NewTcpConnection) -> Result<()> {
        debug!("handle_new_connection -> {:#?}", tcp_connection);

        let stream = self.create_local_stream(&tcp_connection).await?;

        let (sender, receiver) = channel::<Vec<u8>>(1000);

        let new_connection = Connection::new(tcp_connection.connection_id, sender);
        self.connections.insert(new_connection);

        task::spawn(async move { tcp_tunnel(stream, receiver).await });

        Ok(())
    }

    /// Handle New Data messages
    async fn handle_new_data(&mut self, data: TcpData) -> Result<()> {
        debug!("handle_new_data -> id {:#?}", data.connection_id);

        // TODO: "remove -> op -> insert" pattern here, maybe we could improve the overlying
        // abstraction to use something that has mutable access.
        let mut connection = self
            .connections
            .take(&data.connection_id)
            .ok_or(LayerError::NoConnectionId(data.connection_id))?;

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
    fn handle_close(&mut self, close: TcpClose) -> Result<()> {
        debug!("handle_close -> close {:#?}", close);

        let TcpClose { connection_id } = close;

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
