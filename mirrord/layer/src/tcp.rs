use std::io::ErrorKind::ConnectionRefused;
/// Tcp Traffic management, common code for stealing & mirroring
use std::{
    borrow::Borrow,
    collections::HashSet,
    hash::{Hash, Hasher},
    net::SocketAddr,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

use bimap::BiMap;
use mirrord_protocol::{
    tcp::{DaemonTcp, HttpRequest, NewTcpConnection, TcpClose, TcpData},
    ClientMessage, Port, ResponseError,
};
use tokio::{net::TcpStream, sync::mpsc::Sender};
use tracing::{debug, error, info, log::trace, warn};

use crate::{
    detour::DetourGuard,
    error::LayerError,
    socket::{id::SocketId, SocketInformation, CONNECTION_QUEUE},
    LayerError::{PortAlreadyStolen, UnexpectedResponseError},
};

#[derive(Debug)]
pub(crate) enum TcpIncoming {
    Listen(Listen),
    Close(Port),
}

#[derive(Debug, Clone)]
pub(crate) struct Listen {
    pub mirror_port: Port,
    pub requested_port: Port,
    pub ipv6: bool,
    pub id: SocketId,
}

impl PartialEq for Listen {
    fn eq(&self, other: &Self) -> bool {
        self.requested_port == other.requested_port
    }
}

impl Eq for Listen {}

impl Hash for Listen {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.requested_port.hash(state);
    }
}

impl Borrow<Port> for Listen {
    fn borrow(&self) -> &Port {
        &self.requested_port
    }
}

impl From<&Listen> for SocketAddr {
    fn from(listen: &Listen) -> Self {
        let address = if listen.ipv6 {
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), listen.mirror_port)
        } else {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), listen.mirror_port)
        };

        debug_assert_eq!(address.port(), listen.mirror_port);
        address
    }
}

pub(crate) trait TcpHandler {
    const IS_STEAL: bool;
    fn ports(&self) -> &HashSet<Listen>;
    fn ports_mut(&mut self) -> &mut HashSet<Listen>;
    fn port_mapping_ref(&self) -> &BiMap<u16, u16>;

    /// Modify `Listen` to match local port to remote port based on mapping
    /// If no mapping is found, the port is not modified
    fn apply_port_mapping(&self, listen: &mut Listen) {
        if let Some(mapped_port) = self.port_mapping_ref().get_by_left(&listen.requested_port) {
            trace!("mapping port {} to {mapped_port}", &listen.requested_port);
            listen.requested_port = *mapped_port;
        }
    }

    /// Handle a potential [`LayerError::NewConnectionAfterSocketClose`] error.
    fn check_connection_handling_result(res: Result<(), LayerError>) -> Result<(), LayerError> {
        if let Err(LayerError::NewConnectionAfterSocketClose(connection_id)) = res {
            // This can happen and is valid:
            // 1. User app closes socket.
            // 2. mirrord sends `PortUnsubscribe` to agent.
            // 3. Agent sniffs new incoming connection and sends it to the layer.
            // 4. Agent receives `PortUnsubscribe`.
            // Step 2 could even happen after 4.
            if Self::IS_STEAL {
                warn!(
                    "Got incoming tcp connection (mirrord connection id: {connection_id}) after \
                    the application already closed the socket. The connection will not be handled \
                    and will be reset once the agent is informed about the application closing the \
                    socket."
                )
            } else {
                info!(
                    "Got incoming tcp connection (mirrord connection id: {connection_id}) after \
                    the application already closed the socket."
                );
            }
            Ok(())
        } else {
            res
        }
    }

    /// Returns true to let caller know to keep running
    #[tracing::instrument(level = "trace", skip(self))]
    async fn handle_daemon_message(&mut self, message: DaemonTcp) -> Result<(), LayerError> {
        let handled = match message {
            DaemonTcp::NewConnection(tcp_connection) => Self::check_connection_handling_result(
                self.handle_new_connection(tcp_connection).await,
            ),
            DaemonTcp::Data(tcp_data) => self.handle_new_data(tcp_data).await,
            DaemonTcp::Close(tcp_close) => self.handle_close(tcp_close),
            DaemonTcp::SubscribeResult(Ok(port)) => {
                // Added this so tests can know when traffic can be sent
                debug!("daemon subscribed to port {port}.");
                Ok(())
            }
            DaemonTcp::SubscribeResult(Err(ResponseError::PortAlreadyStolen(port))) => {
                error!("Port subscription failed with for port {port}.");
                Err(PortAlreadyStolen(port))
            }
            DaemonTcp::SubscribeResult(Err(other_error)) => {
                error!("Port subscription failed with unexpected error: {other_error}.");
                Err(UnexpectedResponseError(other_error))
            }
            DaemonTcp::HttpRequest(request) => {
                self.handle_http_request(request).await.map_err(From::from)
            }
        };

        debug!("handle_incoming_message -> handled {:#?}", handled);

        handled
    }

    #[tracing::instrument(level = "trace", skip(self, tx))]
    async fn handle_hook_message(
        &mut self,
        message: TcpIncoming,
        tx: &Sender<ClientMessage>,
    ) -> Result<(), LayerError> {
        match message {
            TcpIncoming::Listen(listen) => self.handle_listen(listen, tx).await,
            TcpIncoming::Close(port) => self.handle_server_side_socket_close(port, tx).await,
        }
    }

    /// Handle NewConnection messages
    async fn handle_new_connection(&mut self, conn: NewTcpConnection) -> Result<(), LayerError>;

    /// Connects to the local listening socket, add it to the queue and return the stream.
    /// Find better name
    #[tracing::instrument(level = "trace", skip(self))]
    async fn create_local_stream(
        &mut self,
        tcp_connection: &NewTcpConnection,
    ) -> Result<TcpStream, LayerError> {
        let remote_destination_port = tcp_connection.destination_port;
        let local_destination_port = self
            .port_mapping_ref()
            .get_by_right(&tcp_connection.destination_port)
            .map(|p| {
                trace!("mapping port {} to {p}", &tcp_connection.destination_port);
                *p
            })
            .unwrap_or(tcp_connection.destination_port);

        let listen = self.ports().get(&remote_destination_port).ok_or(
            // New connection arrived after user app closed the socket and the layer removed it
            // from its listening ports.
            LayerError::NewConnectionAfterSocketClose(tcp_connection.connection_id),
        )?;

        let addr: SocketAddr = listen.into();

        let info = SocketInformation::new(
            SocketAddr::new(tcp_connection.remote_address, tcp_connection.source_port),
            // we want local so app won't know we did the mapping.
            SocketAddr::new(tcp_connection.local_address, local_destination_port),
        );

        CONNECTION_QUEUE.add(listen.id, info);

        #[allow(clippy::let_and_return)]
        let tcp_stream = {
            let _ = DetourGuard::new();
            TcpStream::connect(addr).await.map_err(|err| {
                if err.kind() == ConnectionRefused {
                    // A new connection was sent from the agent after the user app already closed
                    // the socket, but before the layer handled the close.
                    LayerError::NewConnectionAfterSocketClose(tcp_connection.connection_id)
                } else {
                    LayerError::from(err)
                }
            })
        };

        tcp_stream
    }

    /// Handle New Data messages
    async fn handle_new_data(&mut self, data: TcpData) -> Result<(), LayerError>;

    /// Handle New Data messages
    async fn handle_http_request(&mut self, request: HttpRequest) -> Result<(), LayerError>;

    /// Handle connection close from the client side (notified via agent message).
    fn handle_close(&mut self, close: TcpClose) -> Result<(), LayerError>;

    /// Handle the user application closing the socket.
    async fn handle_server_side_socket_close(
        &mut self,
        port: Port,
        tx: &Sender<ClientMessage>,
    ) -> Result<(), LayerError>;

    /// Handle listen request
    async fn handle_listen(
        &mut self,
        listen: Listen,
        tx: &Sender<ClientMessage>,
    ) -> Result<(), LayerError>;
}
