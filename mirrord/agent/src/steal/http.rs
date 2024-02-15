//! Home for most of the HTTP stealer implementation / modules.
//!
//! # [`HttpV`]
//!
//! Helper trait to deal with [`hyper`] differences betwen HTTP/1 and HTTP/2 types.
//!
//! # [`HttpVersion`]
//!
//! # [`HttpFilterManager`]
//!
//! Holds the filters for a port we're stealing HTTP traffic on.
use std::{net::SocketAddr, sync::Arc};

use bytes::Bytes;
use dashmap::DashMap;
use http_body_util::combinators::BoxBody;
use hyper::{body::Incoming, Request};
use mirrord_protocol::ConnectionId;
use tokio::{
    net::TcpStream,
    sync::{mpsc::Sender, oneshot},
};

use self::{
    error::HttpTrafficError,
    filter::{H2_PREFACE, MINIMAL_HEADER_SIZE},
    hyper_handler::RawHyperConnection,
    reversible_stream::ReversibleStream,
};
use crate::{steal::HandlerHttpRequest, util::ClientId};

pub(crate) mod error;
mod filter;
mod hyper_handler;
mod reversible_stream;
mod v1;
mod v2;

pub(crate) use filter::{filter_task, HttpFilter};

pub(crate) type Response<B = BoxBody<Bytes, HttpTrafficError>> = hyper::Response<B>;

/// Handy alias due to [`ReversibleStream`] being generic, avoiding value mismatches.
type DefaultReversibleStream = ReversibleStream<MINIMAL_HEADER_SIZE>;

/// Unifies [`hyper`] handling for HTTP/1 and HTTP/2.
///
/// # Details
///
/// As most of the `hyper` types around HTTP/1 and HTTP/2 are different, and do not share a trait,
/// we use [`HttpV`] to create a shared implementation that is used by
/// [`hyper_handler::HyperHandler`].
trait HttpV {
    /// Type for hyper's `SendRequest`.
    ///
    /// It's a different type for HTTP/1 ([`http1::SendRequest`]) and
    /// HTTP/2 ([`http2::SendRequest`]).
    ///
    /// [`http1::SendRequest`]: hyper::client::conn::http1::SendRequest
    /// [`http2::SendRequest`]: hyper::client::conn::http2::SendRequest
    type Sender;

    /// Handles the HTTP connection accordingly to its version.
    ///
    /// See [`filter_task`] for more details.
    async fn serve_connection(
        stream: DefaultReversibleStream,
        original_destination: SocketAddr,
        connection_id: ConnectionId,
        filters: Arc<DashMap<ClientId, HttpFilter>>,
        matched_tx: Sender<HandlerHttpRequest>,
        connection_close_sender: Sender<ConnectionId>,
    ) -> Result<(), HttpTrafficError>;

    /// Performs a client handshake with `target_stream`, creating an HTTP connection.
    ///
    /// # HTTP/1
    ///
    /// The HTTP/1 connection is a bit more involved, as we have to deal with potential `UPGRADE`
    /// requests.
    ///
    /// We do this manually, by keeping the connection alive with `poll_without_shutdown`, and by
    /// sending it as a [`RawHyperConnection`] through `upgrade_tx` to the HTTP stealer handler.
    async fn connect(
        target_stream: TcpStream,
        upgrade_tx: Option<oneshot::Sender<RawHyperConnection>>,
    ) -> Result<Self::Sender, HttpTrafficError>;

    /// Sends the request to the original destination with [`HttpV::Sender`].
    async fn send_request(
        sender: &mut Self::Sender,
        request: Request<Incoming>,
    ) -> Result<Response, HttpTrafficError>;

    /// Returns `true` if this [`Request`] contains an `UPGRADE` header.
    ///
    /// # Warning:
    ///
    /// This implementation should **always** return `false` for HTTP/2.
    fn is_upgrade(r: &Request<Incoming>) -> bool;
}

/// Identifies a message as being HTTP or not.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum HttpVersion {
    /// HTTP/1
    #[default]
    V1,

    /// HTTP/2
    V2,

    /// Handled as a special passthrough case, where the captured stream just forwards messages to
    /// their original destination (and vice-versa).
    NotHttp,
}

impl HttpVersion {
    /// Checks if `buffer` contains a valid HTTP/1.x request, or if it could be an HTTP/2 request by
    /// comparing it with a slice of [`H2_PREFACE`].
    #[tracing::instrument(level = "trace")]
    pub(crate) fn new(buffer: &[u8]) -> Self {
        let mut empty_headers = [httparse::EMPTY_HEADER; 0];

        if buffer.len() < MINIMAL_HEADER_SIZE {
            Self::NotHttp
        } else if buffer == &H2_PREFACE[..MINIMAL_HEADER_SIZE] {
            Self::V2
        } else if matches!(
            httparse::Request::new(&mut empty_headers).parse(buffer),
            Ok(_) | Err(httparse::Error::TooManyHeaders)
        ) {
            Self::V1
        } else {
            Self::NotHttp
        }
    }
}
