use std::{
    borrow::Borrow,
    collections::HashSet,
    hash::{Hash, Hasher},
    time::Duration,
};

use bimap::BiMap;
use mirrord_protocol::{
    tcp::{HttpRequest, LayerTcp, NewTcpConnection, TcpClose, TcpData},
    ClientMessage, ConnectionId,
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
use tracing::{debug, error, info, trace, warn};

use crate::{
    error::{LayerError, Result},
    tcp::{Listen, TcpHandler},
};

#[tracing::instrument(level = "trace", skip(remote_stream))]
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
                        info!("Failed reading local_stream with {:#?}", fail);
                        break;
                    }
                    Ok(read_amount) if read_amount == 0 => {
                        trace!("tcp_tunnel -> exiting due to local stream closed!");
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
    id: ConnectionId,
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
    pub fn new(id: ConnectionId, writer: Sender<Vec<u8>>) -> Self {
        Self { id, writer }
    }

    pub async fn write(&mut self, data: Vec<u8>) -> Result<()> {
        self.writer.send(data).await.map_err(From::from)
    }
}

impl Borrow<ConnectionId> for Connection {
    fn borrow(&self) -> &ConnectionId {
        &self.id
    }
}

/// Handles traffic mirroring
#[derive(Default)]
pub struct TcpMirrorHandler {
    ports: HashSet<Listen>,
    connections: HashSet<Connection>,
    /// LocalPort:RemotePort mapping.
    port_mapping: BiMap<u16, u16>,
}

impl TcpMirrorHandler {
    pub fn new(port_mapping: BiMap<u16, u16>) -> Self {
        Self {
            port_mapping,
            ..Default::default()
        }
    }
}

impl TcpHandler for TcpMirrorHandler {
    /// Handle NewConnection messages
    #[tracing::instrument(level = "trace", skip(self))]
    async fn handle_new_connection(&mut self, tcp_connection: NewTcpConnection) -> Result<()> {
        let stream = self.create_local_stream(&tcp_connection).await?;
        debug!("Handling new connection: local stream: {:?}.", &stream);

        let (sender, receiver) = channel::<Vec<u8>>(1000);

        let new_connection = Connection::new(tcp_connection.connection_id, sender);
        self.connections.insert(new_connection);

        task::spawn(async move { tcp_tunnel(stream, receiver).await });

        Ok(())
    }

    /// Handle New Data messages
    #[tracing::instrument(level = "trace", skip(self), fields(data = data.connection_id))]
    async fn handle_new_data(&mut self, data: TcpData) -> Result<()> {
        // TODO: "remove -> op -> insert" pattern here, maybe we could improve the overlying
        // abstraction to use something that has mutable access.
        if let Some(mut connection) = self.connections.take(&data.connection_id) {
            debug!(
                "handle_new_data -> writing {:#?} bytes to id {:#?}",
                data.bytes.len(),
                connection.id
            );
            // TODO: Due to the above, if we fail here this connection is leaked (-agent won't be
            // told that we just removed it).
            connection.write(data.bytes).await?;

            self.connections.insert(connection);
            debug!("handle_new_data -> success");
        } else {
            // in case connection not found, this might be due to different state between
            // remote socket and local socket, so we ignore it
            trace!("connection not found {:#?}, ignoring", data.connection_id);
        }
        Ok(())
    }

    /// Handle connection close
    #[tracing::instrument(level = "trace", skip(self))]
    fn handle_close(&mut self, close: TcpClose) -> Result<()> {
        let TcpClose { connection_id } = close;

        // Dropping the connection -> Sender drops -> Receiver disconnects -> tcp_tunnel ends
        self.connections.remove(&connection_id);

        // We ignore if the connection was not found, it might have been closed already
        Ok(())
    }

    fn ports(&self) -> &HashSet<Listen> {
        &self.ports
    }

    fn ports_mut(&mut self) -> &mut HashSet<Listen> {
        &mut self.ports
    }

    fn port_mapping_ref(&self) -> &BiMap<u16, u16> {
        &self.port_mapping
    }

    #[tracing::instrument(level = "trace", skip(self, tx))]
    async fn handle_listen(
        &mut self,
        mut listen: Listen,
        tx: &Sender<ClientMessage>,
    ) -> Result<()> {
        self.apply_port_mapping(&mut listen);
        let request_port = listen.requested_port;

        if self.ports_mut().replace(listen).is_some() {
            // This can also be because we currently don't inform the tcp handler when an app closes
            // a socket (stops listening).
            info!("Received listen hook message for port {request_port} while already listening. Might be on different address",);
            return Ok(());
        }

        tx.send(ClientMessage::Tcp(LayerTcp::PortSubscribe(request_port)))
            .await
            .map_err(From::from)
    }

    /// This should never be called here. This exists because we have one trait for mirror and
    /// steal, but steal has some extra messages that should not be used with mirror.
    async fn handle_http_request(
        &mut self,
        _request: HttpRequest,
    ) -> std::result::Result<(), LayerError> {
        error!("Error: Mirror handler received http request.");
        debug_assert!(false);
        Ok(())
    }
}
