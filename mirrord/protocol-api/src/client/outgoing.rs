use std::{
    collections::{HashMap, VecDeque},
    net::SocketAddr,
};

use mirrord_protocol::{
    ClientMessage, DaemonMessage, RemoteResult,
    outgoing::{
        DaemonConnect, DaemonConnectV2, LayerConnect, LayerConnectV2, OUTGOING_CONNECT_V2,
        SocketAddress, UnixAddr,
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        udp::{DaemonUdpOutgoing, LayerUdpOutgoing},
    },
    uid::Uid,
};
use strum::VariantArray;
use strum_macros::VariantArray;

use crate::{
    client::{
        enum_map::{EnumKey, EnumMap},
        error::{ClientError, TaskError, TaskResult},
        outbox::OutBox,
        request::ResponseOneshot,
        tunnels::{TrafficTunnels, TunnelId, TunnelType},
    },
    shrinkable::Shrinkable,
    traffic::TunneledOutgoing,
};

/// Mode of an outgoing connection.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy, VariantArray)]
#[repr(u8)]
pub enum OutgoingMode {
    Tcp,
    Udp,
}

impl OutgoingMode {
    /// Produces a connect request [`ClientMessage`].
    fn connect_request_message(
        self,
        remote_address: SocketAddress,
        uid: Option<Uid>,
    ) -> ClientMessage {
        match (self, uid) {
            (Self::Tcp, None) => {
                ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(LayerConnect {
                    remote_address,
                }))
            }
            (Self::Tcp, Some(uid)) => {
                ClientMessage::TcpOutgoing(LayerTcpOutgoing::ConnectV2(LayerConnectV2 {
                    uid,
                    remote_address,
                }))
            }
            (Self::Udp, None) => {
                ClientMessage::UdpOutgoing(LayerUdpOutgoing::Connect(LayerConnect {
                    remote_address,
                }))
            }
            (Self::Udp, Some(uid)) => {
                ClientMessage::UdpOutgoing(LayerUdpOutgoing::ConnectV2(LayerConnectV2 {
                    uid,
                    remote_address,
                }))
            }
        }
    }

    /// Produces a [`TaskError::unexpected_message`].
    fn unexpected_response(
        self,
        connect: RemoteResult<DaemonConnect>,
        uid: Option<Uid>,
    ) -> TaskError {
        let message = match (self, uid) {
            (Self::Tcp, None) => DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(connect)),
            (Self::Tcp, Some(uid)) => {
                DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::ConnectV2(DaemonConnectV2 {
                    connect,
                    uid,
                }))
            }
            (Self::Udp, None) => DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Connect(connect)),
            (Self::Udp, Some(uid)) => {
                DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::ConnectV2(DaemonConnectV2 {
                    connect,
                    uid,
                }))
            }
        };
        TaskError::unexpected_message(&message)
    }
}

impl From<OutgoingMode> for TunnelType {
    fn from(value: OutgoingMode) -> Self {
        match value {
            OutgoingMode::Tcp => Self::OutgoingTcp,
            OutgoingMode::Udp => Self::OutgoingUdp,
        }
    }
}

impl EnumKey for OutgoingMode {
    fn into_index(self) -> usize {
        self as usize
    }
}

pub type OutgoingModeMap<T> = EnumMap<OutgoingMode, T, { OutgoingMode::VARIANTS.len() }>;

/// Manages state of the outgoing traffic feature.
///
/// Stores [`ResponseOneshot`]s for pending connect requests ([`Self::handle_connect_ip`],
/// [`Self::handle_connect_unix`]).
#[derive(Debug)]
pub enum Outgoing {
    /// V1 protocol, with connection requests processed sequentially (but independently for TCP and
    /// UDP).
    V1(OutgoingModeMap<VecDeque<EitherOneshot>>),
    /// V2 protocol, with connection requests processed concurrently and identified by [`Uid`]s.
    V2(HashMap<Uid, EitherOneshot>),
}

impl Outgoing {
    /// Creates a new instance.
    ///
    /// # Params
    /// * `protocol_version` determines which version of outgoing connect we use
    pub fn new(protocol_version: &semver::Version) -> Self {
        if OUTGOING_CONNECT_V2.matches(protocol_version) {
            Self::V2(Default::default())
        } else {
            Self::V1(Default::default())
        }
    }

    /// Clears the internal state and sends [`ClientError::ConnectionLost`] to all pending
    /// [`ResponseOneshot`]s.
    pub fn server_connection_lost(&mut self, error: &TaskError) {
        match self {
            Self::V1(pending) => std::mem::take(pending)
                .into_values()
                .into_iter()
                .flatten()
                .for_each(|tx| tx.fail(ClientError::ConnectionLost(error.clone()))),
            Self::V2(pending) => {
                std::mem::take(pending)
                    .into_values()
                    .for_each(|tx| tx.fail(ClientError::ConnectionLost(error.clone())));
            }
        }
    }

    /// Handles client's request to make an outgoing TCP or UDP connection.
    ///
    /// Produces a [`ClientMessage`] to be sent to the server
    /// and stores the given [`ResponseOneshot`] as waiting for the connect response.
    pub fn handle_connect_ip(
        &mut self,
        addr: SocketAddr,
        mode: OutgoingMode,
        response_tx: ResponseOneshot<TunneledOutgoing<SocketAddr>>,
    ) -> OutBox {
        let remote_address = SocketAddress::Ip(addr);
        match self {
            Self::V1(pending) => {
                pending[mode].push_back(EitherOneshot::Ip(response_tx));
                mode.connect_request_message(remote_address, None).into()
            }
            Self::V2(pending) => {
                let uid = Uid::new_v4();
                pending.insert(uid, EitherOneshot::Ip(response_tx));
                mode.connect_request_message(remote_address, Some(uid))
                    .into()
            }
        }
    }

    /// Handles client's request to make an outgoing UNIX STREAM connection.
    ///
    /// Produces a [`ClientMessage`] to be sent to the server
    /// and stores the given [`ResponseOneshot`] as waiting for the connect response.
    pub fn handle_connect_unix(
        &mut self,
        addr: UnixAddr,
        response_tx: ResponseOneshot<TunneledOutgoing<UnixAddr>>,
    ) -> OutBox {
        let remote_address = SocketAddress::Unix(addr);
        match self {
            Self::V1(pending) => {
                pending[OutgoingMode::Tcp].push_back(EitherOneshot::Unix(response_tx));
                OutgoingMode::Tcp
                    .connect_request_message(remote_address, None)
                    .into()
            }
            Self::V2(pending) => {
                let uid = Uid::new_v4();
                pending.insert(uid, EitherOneshot::Unix(response_tx));
                OutgoingMode::Tcp
                    .connect_request_message(remote_address, Some(uid))
                    .into()
            }
        }
    }

    pub async fn handle_server_message_tcp(
        &mut self,
        message: DaemonTcpOutgoing,
        tunnels: &mut TrafficTunnels,
    ) -> TaskResult<OutBox> {
        let mode = OutgoingMode::Tcp;

        let out = match message {
            DaemonTcpOutgoing::Connect(result) => {
                self.handle_connect_response(result, None, mode, tunnels)?;
                Default::default()
            }
            DaemonTcpOutgoing::ConnectV2(connect) => {
                self.handle_connect_response(connect.connect, Some(connect.uid), mode, tunnels)?;
                Default::default()
            }
            DaemonTcpOutgoing::Read(Ok(read)) => {
                let id = TunnelId(mode.into(), read.connection_id);
                if read.bytes.is_empty() {
                    tunnels.server_shutdown(id)
                } else {
                    tunnels.server_data(id, read.bytes.0).await
                }
            }
            DaemonTcpOutgoing::Close(id) => {
                tunnels.server_close(TunnelId(mode.into(), id));
                Default::default()
            }
            message @ DaemonTcpOutgoing::Read(Err(..)) => {
                return Err(TaskError::unexpected_message(&DaemonMessage::TcpOutgoing(
                    message,
                )));
            }
        };

        Ok(out)
    }

    pub async fn handle_server_message_udp(
        &mut self,
        message: DaemonUdpOutgoing,
        tunnels: &mut TrafficTunnels,
    ) -> TaskResult<OutBox> {
        let mode = OutgoingMode::Udp;

        let out = match message {
            DaemonUdpOutgoing::Connect(result) => {
                self.handle_connect_response(result, None, mode, tunnels)?;
                Default::default()
            }
            DaemonUdpOutgoing::ConnectV2(connect) => {
                self.handle_connect_response(connect.connect, Some(connect.uid), mode, tunnels)?;
                Default::default()
            }
            DaemonUdpOutgoing::Read(Ok(read)) => {
                let id = TunnelId(mode.into(), read.connection_id);
                if read.bytes.is_empty() {
                    tunnels.server_shutdown(id)
                } else {
                    tunnels.server_data(id, read.bytes.0).await
                }
            }
            DaemonUdpOutgoing::Close(id) => {
                tunnels.server_close(TunnelId(mode.into(), id));
                Default::default()
            }
            message @ DaemonUdpOutgoing::Read(Err(..)) => {
                return Err(TaskError::unexpected_message(&DaemonMessage::UdpOutgoing(
                    message,
                )));
            }
        };

        Ok(out)
    }

    /// Handles [`DaemonConnect`] result from the server.
    fn handle_connect_response(
        &mut self,
        result: RemoteResult<DaemonConnect>,
        uid: Option<Uid>,
        mode: OutgoingMode,
        tunnels: &mut TrafficTunnels,
    ) -> TaskResult<()> {
        let tx = match (uid, self) {
            (None, Self::V1(pending)) => pending[mode]
                .pop_front()
                .inspect(|_| pending[mode].smart_shrink()),
            (Some(uid), Self::V2(pending)) => {
                pending.remove(&uid).inspect(|_| pending.smart_shrink())
            }
            _ => None,
        };
        let Some(tx) = tx else {
            return Err(mode.unexpected_response(result, uid));
        };

        let connect = match result {
            Ok(connect) => connect,
            Err(error) => {
                tx.fail(ClientError::Response(error));
                return Ok(());
            }
        };

        let data = tunnels.server_connection(TunnelId(mode.into(), connect.connection_id))?;
        match (tx, connect) {
            (
                EitherOneshot::Ip(tx),
                DaemonConnect {
                    local_address: SocketAddress::Ip(local),
                    remote_address: SocketAddress::Ip(remote),
                    ..
                },
            ) => {
                let tunneled = TunneledOutgoing {
                    remote_local_addr: local,
                    remote_peer_addr: remote,
                    data,
                };
                let _ = tx.send(Ok(tunneled));
            }

            (
                EitherOneshot::Unix(tx),
                DaemonConnect {
                    local_address: SocketAddress::Unix(local),
                    remote_address: SocketAddress::Unix(remote),
                    ..
                },
            ) => {
                let tunneled = TunneledOutgoing {
                    remote_local_addr: local,
                    remote_peer_addr: remote,
                    data,
                };
                let _ = tx.send(Ok(tunneled));
            }

            (.., connect) => return Err(mode.unexpected_response(Ok(connect), uid)),
        }

        Ok(())
    }
}

#[derive(Debug)]
pub enum EitherOneshot {
    Ip(ResponseOneshot<TunneledOutgoing<SocketAddr>>),
    Unix(ResponseOneshot<TunneledOutgoing<UnixAddr>>),
}

impl EitherOneshot {
    fn fail(self, error: ClientError) {
        match self {
            EitherOneshot::Ip(tx) => {
                tx.send(Err(error)).ok();
            }
            EitherOneshot::Unix(tx) => {
                tx.send(Err(error)).ok();
            }
        }
    }
}
