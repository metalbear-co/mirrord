/// Tcp Traffic management, common code for stealing & mirroring
use std::{
    borrow::Borrow,
    collections::HashSet,
    hash::{Hash, Hasher},
    net::SocketAddr,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    os::unix::io::RawFd,
};

use async_trait::async_trait;
use mirrord_protocol::{
    tcp::{DaemonTcp, NewTcpConnection, TcpClose, TcpData},
    ClientCodec, Port,
};
use tokio::net::TcpStream;
use tracing::{debug, trace};

use crate::{
    detour::DetourGuard,
    error::LayerError,
    socket::{SocketInformation, CONNECTION_QUEUE},
};

pub(crate) mod outgoing;

#[derive(Debug)]
pub(crate) enum HookMessageTcp {
    Listen(Listen),
}

#[derive(Debug, Clone)]
pub(crate) struct Listen {
    pub mirror_port: Port,
    pub requested_port: Port,
    pub ipv6: bool,
    pub fd: RawFd,
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

#[async_trait]
pub(crate) trait TcpHandler {
    fn ports(&self) -> &HashSet<Listen>;
    fn ports_mut(&mut self) -> &mut HashSet<Listen>;

    /// Returns true to let caller know to keep running
    async fn handle_daemon_message(&mut self, message: DaemonTcp) -> Result<(), LayerError> {
        debug!("handle_incoming_message -> message {:?}", message);

        let handled = match message {
            DaemonTcp::NewConnection(tcp_connection) => {
                self.handle_new_connection(tcp_connection).await
            }
            DaemonTcp::Data(tcp_data) => self.handle_new_data(tcp_data).await,
            DaemonTcp::Close(tcp_close) => self.handle_close(tcp_close),
            DaemonTcp::Subscribed => {
                // Added this so tests can know when traffic can be sent
                debug!("daemon subscribed");
                Ok(())
            }
        };

        debug!("handle_incoming_message -> handled {:#?}", handled);

        handled
    }

    async fn handle_hook_message(
        &mut self,
        message: HookMessageTcp,
        codec: &mut actix_codec::Framed<
            impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
            ClientCodec,
        >,
    ) -> Result<(), LayerError> {
        match message {
            HookMessageTcp::Listen(listen) => self.handle_listen(listen, codec).await,
        }
    }

    /// Handle NewConnection messages
    async fn handle_new_connection(&mut self, conn: NewTcpConnection) -> Result<(), LayerError>;

    /// Connects to the local listening socket, add it to the queue and return the stream.
    /// Find better name
    async fn create_local_stream(
        &mut self,
        tcp_connection: &NewTcpConnection,
    ) -> Result<TcpStream, LayerError> {
        trace!(
            "create_local_stream -> tcp_connection {:#?}",
            tcp_connection
        );
        let destination_port = tcp_connection.destination_port;

        let listen = self
            .ports()
            .get(&destination_port)
            .ok_or(LayerError::PortNotFound(destination_port))?;

        let addr: SocketAddr = listen.into();

        let info = SocketInformation::new(SocketAddr::new(
            tcp_connection.address,
            tcp_connection.source_port,
        ));
        {
            CONNECTION_QUEUE.lock().unwrap().add(&listen.fd, info);
        }

        #[allow(clippy::let_and_return)]
        let tcp_stream = {
            let _ = DetourGuard::new();
            TcpStream::connect(addr).await.map_err(From::from)
        };

        tcp_stream
    }

    /// Handle New Data messages
    async fn handle_new_data(&mut self, data: TcpData) -> Result<(), LayerError>;

    /// Handle connection close
    fn handle_close(&mut self, close: TcpClose) -> Result<(), LayerError>;

    /// Handle listen request
    async fn handle_listen(
        &mut self,
        listen: Listen,
        codec: &mut actix_codec::Framed<
            impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
            ClientCodec,
        >,
    ) -> Result<(), LayerError>;
}
