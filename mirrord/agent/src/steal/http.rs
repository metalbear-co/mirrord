use std::{net::SocketAddr, sync::Arc};

use bytes::Bytes;
use dashmap::DashMap;
use fancy_regex::Regex;
use http_body_util::Full;
use hyper::{body::Incoming, Request, Response};
use mirrord_protocol::ConnectionId;
use tokio::{
    net::TcpStream,
    sync::{mpsc::Sender, oneshot},
};

use self::{
    error::HttpTrafficError, filter::MINIMAL_HEADER_SIZE, hyper_handler::RawHyperConnection,
    reversible_stream::ReversibleStream,
};
use crate::{
    steal::{http::filter::filter_task, HandlerHttpRequest},
    util::ClientId,
};

pub(crate) mod error;
pub(super) mod filter;
pub(super) mod hyper_handler;
pub(super) mod reversible_stream;
pub(super) mod v1;
pub(super) mod v2;

/// Handy alias due to [`ReversibleStream`] being generic, avoiding value mismatches.
pub(super) type DefaultReversibleStream = ReversibleStream<MINIMAL_HEADER_SIZE>;

trait HttpVersionT {
    type Sender;

    async fn connect(
        target_stream: TcpStream,
        upgrade_tx: Option<oneshot::Sender<RawHyperConnection>>,
    ) -> Result<Self::Sender, HttpTrafficError>;

    async fn send_request(
        sender: &mut Self::Sender,
        request: Request<Incoming>,
    ) -> Result<Response<Full<Bytes>>, HttpTrafficError>;

    fn is_upgrade(_: &Request<Incoming>) -> bool {
        false
    }
}

/// Identifies a message as being HTTP or not.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum HttpVersion {
    #[default]
    V1,
    V2,

    /// Handled as a special passthrough case, where the captured stream just forwards messages to
    /// their original destination (and vice-versa).
    NotHttp,
}

impl HttpVersion {
    /// Checks if `buffer` contains a valid HTTP/1.x request, or if it could be an HTTP/2 request by
    /// comparing it with a slice of [`H2_PREFACE`].
    #[tracing::instrument(level = "trace")]
    fn new(buffer: &[u8], h2_preface: &[u8]) -> Self {
        let mut empty_headers = [httparse::EMPTY_HEADER; 0];

        if buffer.len() < MINIMAL_HEADER_SIZE {
            Self::NotHttp
        } else if buffer == h2_preface {
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

/// Created for every new port we want to filter HTTP traffic on.
#[derive(Debug)]
pub(super) struct HttpFilterManager {
    client_filters: Arc<DashMap<ClientId, Regex>>,

    /// We clone this to pass them down to the hyper tasks.
    matched_tx: Sender<HandlerHttpRequest>,
}

impl HttpFilterManager {
    /// Creates a new [`HttpFilterManager`] per port.
    ///
    /// You can't create just an empty [`HttpFilterManager`], as we don't steal traffic on ports
    /// that no client has registered interest in.
    #[tracing::instrument(level = "trace", skip(matched_tx))]
    pub(super) fn new(
        client_id: ClientId,
        filter: Regex,
        matched_tx: Sender<HandlerHttpRequest>,
    ) -> Self {
        let client_filters = Arc::new(DashMap::with_capacity(128));
        client_filters.insert(client_id, filter);

        Self {
            client_filters,
            matched_tx,
        }
    }

    /// Inserts a new client (layer) and its filter.
    ///
    /// [`HttpFilterManager::client_filters`] are shared between hyper tasks, so adding a new one
    /// here will impact the tasks as well.
    #[tracing::instrument(level = "trace", skip(self))]
    pub(super) fn add_client(&mut self, client_id: ClientId, filter: Regex) -> Option<Regex> {
        self.client_filters.insert(client_id, filter)
    }

    /// Removes a client (layer) from [`HttpFilterManager::client_filters`].
    ///
    /// [`HttpFilterManager::client_filters`] are shared between hyper tasks, so removing a client
    /// here will impact the tasks as well.
    #[tracing::instrument(level = "trace", skip(self))]
    pub(super) fn remove_client(&mut self, client_id: &ClientId) -> Option<(ClientId, Regex)> {
        self.client_filters.remove(client_id)
    }

    #[tracing::instrument(level = "trace", skip(self))]
    pub(super) fn contains_client(&self, client_id: &ClientId) -> bool {
        self.client_filters.contains_key(client_id)
    }

    /// Start a [`filter_task`] to handle this new connection.
    #[tracing::instrument(level = "trace", skip(self, original_stream, connection_close_sender))]
    pub(super) async fn new_connection(
        &self,
        original_stream: TcpStream,
        original_address: SocketAddr,
        connection_id: ConnectionId,
        connection_close_sender: Sender<ConnectionId>,
    ) {
        tokio::spawn(filter_task(
            original_stream,
            original_address,
            connection_id,
            self.client_filters.clone(),
            self.matched_tx.clone(),
            connection_close_sender,
        ));
    }

    #[tracing::instrument(level = "trace", skip(self))]
    pub(super) fn is_empty(&self) -> bool {
        self.client_filters.is_empty()
    }
}
