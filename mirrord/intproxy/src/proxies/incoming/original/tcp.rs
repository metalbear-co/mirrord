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
    tcp::{DaemonTcp, HttpRequestFallback, NewTcpConnection, TcpClose, TcpData},
    ClientMessage, Port, ResponseError,
};
use tokio::{net::TcpStream, sync::mpsc::Sender};
use tracing::{debug, error, info, log::trace, warn};

pub trait TcpHandler<const IS_STEAL: bool> {
    /// Handle a potential [`LayerError::NewConnectionAfterSocketClose`] error.
    fn check_connection_handling_result(res: Result<(), LayerError>) -> Result<(), LayerError> {
        if let Err(LayerError::NewConnectionAfterSocketClose(connection_id)) = res {
            // This can happen:
            // 1. User app closes socket.
            // 2. mirrord sends `PortUnsubscribe` to agent.
            // 3. Agent sniffs new incoming connection and sends it to the layer.
            // 4. Agent receives `PortUnsubscribe`.
            // Step 2 could even happen after 4.
            if IS_STEAL {
                warn!(
                    "Got incoming tcp connection (mirrord connection id: {connection_id}) after \
                    the application already closed the socket. The connection will not be handled."
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
    #[tracing::instrument(level = "trace", ret, skip(self))]
    async fn handle_daemon_message(&mut self, message: DaemonTcp) -> Result<(), LayerError> {
        match message {
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
            DaemonTcp::HttpRequest(request) => self
                .handle_http_request(HttpRequestFallback::Fallback(request))
                .await
                .map_err(From::from),
            DaemonTcp::HttpRequestFramed(request) => self
                .handle_http_request(HttpRequestFallback::Framed(request))
                .await
                .map_err(From::from),
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
    async fn handle_http_request(&mut self, request: HttpRequestFallback)
        -> Result<(), LayerError>;

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