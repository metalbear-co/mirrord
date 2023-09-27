use std::{
    env, io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::PathBuf,
};

use mirrord_protocol::{
    outgoing::{
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        udp::{DaemonUdpOutgoing, LayerUdpOutgoing},
        DaemonConnect, DaemonRead, LayerClose, LayerConnect, SocketAddress, UnixAddr,
    },
    ClientMessage, ConnectionId, RemoteResult,
};
use rand::{distributions::Alphanumeric, Rng};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket, UnixListener, UnixStream},
};

use crate::{
    error::{IntProxyError, Result},
    protocol::{
        ConnectTcpOutgoing, ConnectUdpOutgoing, OutgoingConnectResponse, ProxyToLayerMessage,
    },
};

mod interceptor;
pub mod proxy;

pub enum AgentOutgoingMessage {
    Connect(RemoteResult<DaemonConnect>),
    Read(RemoteResult<DaemonRead>),
    Close(ConnectionId),
}

#[async_trait::async_trait]
pub trait NetProtocolHandler: 'static + Send + Sync {
    type Listener: Listener<Socket = Self::Socket>;
    type Socket: Socket;
    type LayerConnectRequest;
    type AgentMessage;

    fn make_close_message(connection_id: ConnectionId) -> ClientMessage;

    fn make_connect_message(remote_address: SocketAddress) -> ClientMessage;

    fn unwrap_layer_connect_request(request: Self::LayerConnectRequest) -> SocketAddress;

    fn wrap_layer_connect_response(
        response: RemoteResult<OutgoingConnectResponse>,
    ) -> ProxyToLayerMessage;

    fn unwrap_agent_message(message: Self::AgentMessage) -> AgentOutgoingMessage;
}

#[async_trait::async_trait]
pub trait Listener: 'static + Send + Sync + Sized {
    type Socket: Socket;

    async fn create(for_remote_address: SocketAddress) -> Result<Self>;

    fn local_address(&self) -> Result<SocketAddress>;

    async fn accept(self) -> Result<Self::Socket>;
}

#[async_trait::async_trait]
pub trait Socket: 'static + Send + Sync {
    async fn send(&mut self, bytes: &[u8]) -> Result<()>;

    async fn receive(&mut self) -> Result<Vec<u8>>;
}

pub struct DatagramHandler;

pub struct DatagramSocket {
    socket: UdpSocket,
    peer: Option<SocketAddr>,
    buffer: Vec<u8>,
}

impl NetProtocolHandler for DatagramHandler {
    type Listener = DatagramSocket;
    type Socket = DatagramSocket;
    type LayerConnectRequest = ConnectUdpOutgoing;
    type AgentMessage = DaemonUdpOutgoing;

    fn make_close_message(connection_id: ConnectionId) -> ClientMessage {
        ClientMessage::UdpOutgoing(LayerUdpOutgoing::Close(LayerClose { connection_id }))
    }

    fn make_connect_message(remote_address: SocketAddress) -> ClientMessage {
        ClientMessage::UdpOutgoing(LayerUdpOutgoing::Connect(LayerConnect { remote_address }))
    }

    fn unwrap_layer_connect_request(request: Self::LayerConnectRequest) -> SocketAddress {
        request.remote_address
    }

    fn wrap_layer_connect_response(
        response: RemoteResult<OutgoingConnectResponse>,
    ) -> ProxyToLayerMessage {
        ProxyToLayerMessage::ConnectUdpOutgoing(response)
    }

    fn unwrap_agent_message(message: Self::AgentMessage) -> AgentOutgoingMessage {
        match message {
            DaemonUdpOutgoing::Close(close) => AgentOutgoingMessage::Close(close),
            DaemonUdpOutgoing::Connect(connect) => AgentOutgoingMessage::Connect(connect),
            DaemonUdpOutgoing::Read(read) => AgentOutgoingMessage::Read(read),
        }
    }
}

#[async_trait::async_trait]
impl Listener for DatagramSocket {
    type Socket = Self;

    fn local_address(&self) -> Result<SocketAddress> {
        let addr = self.socket.local_addr()?;

        Ok(addr.into())
    }

    async fn create(for_remote_address: SocketAddress) -> Result<Self> {
        let bind_address = match for_remote_address {
            SocketAddress::Ip(inner) => match inner.ip() {
                IpAddr::V4(..) => SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
                IpAddr::V6(..) => SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0),
            },
            SocketAddress::Unix(_) => {
                tracing::error!("attempted to send datagrams over unix socket");
                return Err(IntProxyError::DatagramOverUnix);
            }
        };

        let socket = UdpSocket::bind(bind_address).await?;

        Ok(Self {
            peer: None,
            socket,
            buffer: vec![0; 1500],
        })
    }

    async fn accept(self) -> Result<Self::Socket> {
        Ok(self)
    }
}

#[async_trait::async_trait]
impl Socket for DatagramSocket {
    async fn send(&mut self, bytes: &[u8]) -> Result<()> {
        let Some(peer) = self.peer else {
            panic!("cannot send data until peer is known");
        };

        let bytes_sent = self.socket.send_to(bytes, peer).await?;

        if bytes_sent != bytes.len() {
            Err(io::Error::new(
                io::ErrorKind::Other,
                "failed to send all bytes",
            ))?;
        }

        Ok(())
    }

    async fn receive(&mut self) -> Result<Vec<u8>> {
        let (received, peer) = self.socket.recv_from(&mut self.buffer).await?;

        if self.peer.is_none() {
            self.peer.replace(peer);
            self.socket.connect(peer).await?;
        }

        let bytes = self.buffer.get(..received).unwrap().to_vec();

        Ok(bytes)
    }
}

pub struct StreamHandler;

impl StreamHandler {
    /// For unix socket addresses, relative to the temp dir.
    const UNIX_STREAMS_DIRNAME: &'static str = "mirrord-unix-sockets";

    async fn generate_uds_path() -> Result<PathBuf> {
        let tmp_dir = env::temp_dir().join(Self::UNIX_STREAMS_DIRNAME);
        if !tmp_dir.exists() {
            fs::create_dir_all(&tmp_dir).await?;
        }

        let random_string: String = rand::thread_rng()
            .sample_iter(Alphanumeric)
            .take(16)
            .map(char::from)
            .collect();

        Ok(tmp_dir.join(random_string))
    }
}

pub enum StreamListener {
    Tcp(TcpListener),
    Unix(UnixListener),
}

pub enum StreamSocket {
    Tcp { stream: TcpStream, buffer: Vec<u8> },
    Unix { stream: UnixStream, buffer: Vec<u8> },
}

#[async_trait::async_trait]
impl Listener for StreamListener {
    type Socket = StreamSocket;

    fn local_address(&self) -> Result<SocketAddress> {
        match self {
            Self::Tcp(tcp) => tcp.local_addr().map(Into::into).map_err(Into::into),
            Self::Unix(unix) => {
                let path = unix
                    .local_addr()?
                    .as_pathname()
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::Other, "no pathname found for unix listener")
                    })?
                    .to_path_buf();
                Ok(SocketAddress::Unix(UnixAddr::Pathname(path)))
            }
        }
    }

    async fn create(for_remote_address: SocketAddress) -> Result<Self> {
        match for_remote_address {
            SocketAddress::Ip(inner) => {
                let addr = match inner.ip() {
                    IpAddr::V4(..) => SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
                    IpAddr::V6(..) => SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0),
                };

                let listener = TcpListener::bind(addr).await?;

                Ok(Self::Tcp(listener))
            }
            SocketAddress::Unix(_) => {
                let path = StreamHandler::generate_uds_path().await?;

                let listener = UnixListener::bind(path)?;

                Ok(Self::Unix(listener))
            }
        }
    }

    async fn accept(self) -> Result<Self::Socket> {
        match self {
            Self::Tcp(tcp) => {
                let (stream, _) = tcp.accept().await?;
                Ok(StreamSocket::Tcp {
                    stream,
                    buffer: vec![0; 1024],
                })
            }
            Self::Unix(unix) => {
                let (stream, _) = unix.accept().await?;
                Ok(StreamSocket::Unix {
                    stream,
                    buffer: vec![0; 1024],
                })
            }
        }
    }
}

#[async_trait::async_trait]
impl Socket for StreamSocket {
    async fn send(&mut self, bytes: &[u8]) -> Result<()> {
        match self {
            Self::Tcp { stream, .. } => stream.write_all(bytes).await?,
            Self::Unix { stream, .. } => stream.write_all(bytes).await?,
        }

        Ok(())
    }

    async fn receive(&mut self) -> Result<Vec<u8>> {
        match self {
            Self::Tcp { stream, buffer } => {
                let received = stream.read(buffer).await?;
                Ok(buffer.get(..received).unwrap().to_vec())
            }
            Self::Unix { stream, buffer } => {
                let received = stream.read(buffer).await?;
                Ok(buffer.get(..received).unwrap().to_vec())
            }
        }
    }
}

#[async_trait::async_trait]
impl NetProtocolHandler for StreamHandler {
    type Listener = StreamListener;
    type Socket = StreamSocket;
    type LayerConnectRequest = ConnectTcpOutgoing;
    type AgentMessage = DaemonTcpOutgoing;

    fn make_close_message(connection_id: ConnectionId) -> ClientMessage {
        ClientMessage::TcpOutgoing(LayerTcpOutgoing::Close(LayerClose { connection_id }))
    }

    fn make_connect_message(remote_address: SocketAddress) -> ClientMessage {
        ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(LayerConnect { remote_address }))
    }

    fn unwrap_layer_connect_request(request: Self::LayerConnectRequest) -> SocketAddress {
        request.remote_address
    }

    fn wrap_layer_connect_response(
        response: RemoteResult<OutgoingConnectResponse>,
    ) -> ProxyToLayerMessage {
        ProxyToLayerMessage::ConnectTcpOutgoing(response)
    }

    fn unwrap_agent_message(message: Self::AgentMessage) -> AgentOutgoingMessage {
        match message {
            DaemonTcpOutgoing::Close(close) => AgentOutgoingMessage::Close(close),
            DaemonTcpOutgoing::Connect(connect) => AgentOutgoingMessage::Connect(connect),
            DaemonTcpOutgoing::Read(read) => AgentOutgoingMessage::Read(read),
        }
    }
}
