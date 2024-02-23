use http_body_util::BodyExt;
use hyper::{body::Incoming, Request};
use mirrord_protocol::{
    tcp::{
        DaemonTcp, HttpRequest, HttpResponseFallback, InternalHttpBody, InternalHttpRequest,
        StealType, TcpData,
    },
    ConnectionId, Port, RequestId,
};
use tokio::{select, sync::mpsc::Sender};
use tokio_util::sync::CancellationToken;

use crate::{error::AgentError, util::ClientId};

pub(super) mod api;
pub(super) mod connection;
mod connections;
pub(super) mod http;
pub(super) mod ip_tables;
mod orig_dst;
mod subscriptions;

/// Commands from the agent that are passed down to the stealer worker, through [`TcpStealerApi`].
///
/// These are the operations that the agent receives from the layer to make the _steal_ feature
/// work.
#[derive(Debug)]
enum Command {
    /// Contains the channel that's used by the stealer worker to respond back to the agent
    /// (stealer -> agent -> layer).
    NewClient(Sender<DaemonTcp>, semver::Version),

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
    HttpResponse(HttpResponseFallback),

    SwitchProtocolVersion(semver::Version),
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
