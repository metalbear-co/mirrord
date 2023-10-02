use std::{
    collections::{HashSet, HashMap},
    hash::Hasher,
    time::Duration, net::SocketAddr,
};

use mirrord_protocol::{
    tcp::{HttpRequestFallback, LayerTcp, NewTcpConnection, TcpClose, TcpData},
    ClientMessage, Port, ConnectionId,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    select,
    sync::mpsc::{channel, Receiver, Sender},
    task,
    time::sleep,
};
use tracing::{debug, error, info, trace, warn};

use crate::{
    error::Result,
    protocol::PortSubscribe, agent_conn::AgentSender,
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
                    Ok(0) => {
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


struct LayerConnection {

}

struct LayerListener {
    address: SocketAddr,
}

pub struct TcpMirrorHandler {
    agent_sender: AgentSender,
    listeners: HashMap<Port, LayerListener>,
    connections: HashMap<ConnectionId, LayerConnection>,
}

impl TcpMirrorHandler {
    pub fn new(agent_sender: AgentSender) -> Self {
        Self {
            agent_sender,
            listeners: Default::default(),
            connections: Default::default(),
        }
    }

    async fn handle_new_connection(&mut self, tcp_connection: NewTcpConnection) -> Result<()> {
        let stream = self.create_local_stream(&tcp_connection).await?;

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

    pub async fn handle_subscribe(
        &mut self,
        subscribe: PortSubscribe,
    ) -> Result<()> {
        let PortSubscribe { port, listening_on } = subscribe;

        let layer_listener = LayerListener { address: listening_on.try_into().unwrap() };
        if self.listeners.insert(port, layer_listener) {
            // Since this struct identifies listening sockets by their port (#1558), we can only
            // forward the incoming traffic to one socket with that port.
            info!(
                "Received listen request for port {port} while already listening. \
                Might be on different address. Sending all incoming traffic to newest socket."
            );

            return Ok(());
        }

        self.agent_sender.send(ClientMessage::Tcp(LayerTcp::PortSubscribe(port))).await.map_err(Into::into)
    }

    async fn handle_server_side_socket_close(
        &mut self,
        port: Port,
        tx: &Sender<ClientMessage>,
    ) -> std::result::Result<(), LayerError> {
        if self.ports_mut().remove(&port) {
            tx.send(ClientMessage::Tcp(LayerTcp::PortUnsubscribe(port)))
                .await
                .map_err(From::from)
        } else {
            // was not listening on closed socket, could be an outgoing socket.
            Ok(())
        }
    }
}
