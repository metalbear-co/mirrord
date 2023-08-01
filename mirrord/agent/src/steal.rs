use std::collections::HashMap;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use hyper::{body::Incoming, http::request, Request, Response};
use mirrord_protocol::{
    tcp::{
        DaemonTcp, HttpRequest, HttpResponse, InternalHttpBody, InternalHttpRequest, StealType,
        TcpData,
    },
    ConnectionId, Port, RequestId,
};
use tokio::{
    net::TcpListener,
    select,
    sync::{mpsc::Sender, oneshot},
};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use self::ip_tables::SafeIpTables;
use crate::{
    error::{AgentError, Result},
    steal::http::error::HttpTrafficError,
    util::{ClientId, IndexAllocator},
};

pub(super) mod api;
pub(super) mod connection;
pub(super) mod http;
pub(super) mod ip_tables;
mod orig_dst;

/// Commands from the agent that are passed down to the stealer worker, through [`TcpStealerApi`].
///
/// These are the operations that the agent receives from the layer to make the _steal_ feature
/// work.
#[derive(Debug)]
enum Command {
    /// Contains the channel that's used by the stealer worker to respond back to the agent
    /// (stealer -> agent -> layer).
    NewClient(Sender<DaemonTcp>),

    /// A layer wants to subscribe to this [`Port`].
    ///
    /// The agent starts stealing traffic on this [`Port`].
    PortSubscribe(StealType),

    /// A layer wants to unsubscribe from this [`Port`].
    ///
    /// The agent stops stealing traffic from this [`Port`].
    PortUnsubscribe(Port),

    /// Part of the [`Drop`] implementation of [`TcpStealerApi`].
    ///
    /// Closes a layer connection, and unsubscribe its ports.
    ClientClose,

    /// A connection here is a pair of ([`ReadHalf`], [`WriteHalf`]) streams that are used to
    /// capture a remote connection (the connection we're stealing data from).
    ConnectionUnsubscribe(ConnectionId),

    /// There is new data in the direction going from the local process to the end-user (Going
    /// via the layer and the agent  local-process -> layer --> agent --> end-user).
    ///
    /// Agent forwards this data to the other side of original connection.
    ResponseData(TcpData),

    /// Response from local app to stolen HTTP request.
    ///
    /// Should be forwarded back to the connection it was stolen from.
    HttpResponse(HttpResponse),
}

/// Association between a client (identified by the `client_id`) and a [`Command`].
///
/// The (agent -> worker) channel uses this, instead of naked [`Command`]s when communicating.
#[derive(Debug)]
pub struct StealerCommand {
    /// Identifies which layer instance is sending the [`Command`].
    client_id: ClientId,

    /// The command message sent from (layer -> agent) to be handled by the stealer worker.
    command: Command,
}

/// A struct that the [`HyperHandler::call`] sends [`TcpConnectionStealer::start`], with a request
/// that matched a filter and should be forwarded to a layer, and sender in which the response to
/// that request can be sent back.
#[derive(Debug)]
pub struct HandlerHttpRequest {
    pub request: MatchedHttpRequest,

    /// For sending the response from the stealer task back to the hyper service.
    /// [`TcpConnectionStealer::start`] -----response to this request-----> [`HyperHandler::call`]
    pub response_tx: oneshot::Sender<Response<BoxBody<Bytes, HttpTrafficError>>>,
}

/// A stolen HTTP request that matched a client's filter. To be sent from the filter code to the
/// connection task to be forwarded to the matching client.
#[derive(Debug)]
pub struct MatchedHttpRequest {
    pub port: Port,
    pub connection_id: ConnectionId,
    pub client_id: ClientId,
    pub request_id: RequestId,
    pub request: Request<Incoming>,
}

impl MatchedHttpRequest {
    async fn into_serializable(self) -> Result<HttpRequest, hyper::Error> {
        let (
            request::Parts {
                method,
                uri,
                version,
                headers,
                ..
            },
            body,
        ) = self.request.into_parts();

        let body = InternalHttpBody::from_body(body).await?;

        let internal_request = InternalHttpRequest {
            method,
            uri,
            headers,
            version,
            body,
        };

        Ok(HttpRequest {
            port: self.port,
            connection_id: self.connection_id,
            request_id: self.request_id,
            internal_request,
        })
    }
}
