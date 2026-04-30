use std::net::SocketAddr;

use mirrord_protocol::{ClientMessage, LogMessage, outgoing::UnixAddr, tcp::HttpFilter};
use tokio::sync::oneshot;
use tokio_stream::wrappers::ReceiverStream;

use crate::{
    client::{
        error::ClientResult, incoming::IncomingMode, outgoing::OutgoingMode, queue_kind::QueueKind,
        simple_request::ResultHandler,
    },
    fifo::{FifoSink, FifoStream},
    traffic::{TunneledIncoming, TunneledOutgoing},
};

/// Type of local requests that can be handled by the [`mirrord_protocol`] client.
///
/// Sent from [`MirrordClient`](crate::client::MirrordClient), and handled by
/// [`ClientTask`](crate::client::task::ClientTask).
pub enum ClientRequest {
    /// Simple request that triggers a response from the server. For example, a
    /// remote DNS resolution.
    ///
    /// Always originates from a [`SimpleRequest`](crate::client::simple_request::SimpleRequest).
    Simple {
        message: ClientMessage,
        queue_kind: QueueKind,
        handler: Box<dyn ResultHandler>,
    },
    /// Simple request that does not trigger any response from the server.
    ///
    /// Always originates from a
    /// [`SimpleRequestNoResponse`](crate::client::simple_request::SimpleRequestNoResponse).
    SimpleNoResponse(ClientMessage),
    /// Request to establish an outgoing TCP/UDP connection (in case of UDP, purely logical).
    ConnectIp {
        addr: SocketAddr,
        mode: OutgoingMode,
        response_tx: ResponseOneshot<TunneledOutgoing<SocketAddr>>,
    },
    /// Request to establish a UNIX socket connection.
    ConnectUnix {
        addr: UnixAddr,
        response_tx: ResponseOneshot<TunneledOutgoing<UnixAddr>>,
    },
    /// Request to create a port subscription.
    SubscribePort {
        port: u16,
        mode: IncomingMode,
        filter: Option<HttpFilter>,
        response_tx: ResponseOneshot<FifoStream<TunneledIncoming>>,
    },
    /// Request for server logs.
    Logs(FifoSink<LogMessage>),
}

/// [`Stream`](futures::stream::Stream) of [`ClientRequest`]s coming from a single
/// [`MirrordClient`](crate::client::MirrordClient).
pub type ClientRequestStream = ReceiverStream<ClientRequest>;

pub type ResponseOneshot<T> = oneshot::Sender<ClientResult<T>>;
