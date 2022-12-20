use std::{collections::HashMap, path::PathBuf};

use http_body_util::BodyExt;
use hyper::{body::Incoming, http::request::Parts, Request};
use mirrord_protocol::{
    tcp::{DaemonTcp, HttpRequest, HttpResponse, InternalHttpRequest, StealType, TcpData},
    ConnectionId, Port,
};
use tokio::{net::TcpListener, select, sync::mpsc::Sender};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use self::ip_tables::SafeIpTables;
use crate::{
    error::{AgentError, Result},
    runtime::set_namespace,
    steal::http_traffic::HttpFilterManager,
    util::{ClientId, IndexAllocator},
};

pub(super) mod api;
pub(super) mod connection;
pub(crate) mod http_traffic;
mod ip_tables;
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

/// A stolen HTTP request. Unlike [`mirrord_protocol::tcp::HttpRequest`], it also contains a
/// ClientId.
#[derive(Debug)]
pub struct StealerHttpRequest {
    pub port: Port,
    pub connection_id: ConnectionId,
    pub client_id: ClientId,
    pub request: Request<Incoming>,
}

impl StealerHttpRequest {
    async fn into_serializable(self) -> Result<HttpRequest, hyper::Error> {
        let (
            Parts {
                method,
                uri,
                version,
                headers,
                extensions: _, // TODO: do we need to use it? There is not such `http_serde` method.
                ..
            },
            body,
        ) = self.request.into_parts();
        let body = body.collect().await?.to_bytes().to_vec();
        let internal_req = InternalHttpRequest {
            method,
            uri,
            headers,
            version,
            body,
        };
        Ok(HttpRequest {
            port: self.port,
            connection_id: self.connection_id,
            request: internal_req,
            request_id: 0, // TODO: Use a real request_id!
        })
    }
}

// // TODO: define in separate file.
// #[derive(Debug)]
// struct HttpFilterManager {
//     /// Channel to send classified requests back to stealer over.
//     request_sender: Sender<StealerHttpRequest>,
// }
//
// impl HttpFilterManager {
//     fn is_empty(&self) -> bool {
//         false // TODO
//     }
//
//     fn has_client(&self, client_id: ClientId) -> bool {
//         false // TODO
//     }
//
//     fn new_connection(
//         &self,
//         stream: TcpStream,
//         connection_id: ConnectionId,
//     ) -> (TcpStream, DuplexStream) {
//         todo!()
//     }
//
//     fn send_response(&self, response: HttpResponse) -> Result<()> {
//         // TODO
//         Ok(())
//     }
//
//     fn insert(&self, client_id: ClientId, regex_str: String) -> Result<()> {
//         // TODO
//         Ok(())
//     }
//
//     fn remove(&self, client_id: ClientId) -> Result<()> {
//         // TODO
//         Ok(())
//     }
// }

/// The subscriptions to steal traffic from a specific port.
#[derive(Debug)]
enum StealSubscription {
    /// All of the port's traffic goes to this single client.
    Unfiltered(ClientId),
    /// This port's traffic is filtered and distributed to clients using a manager.
    HttpFiltered(HttpFilterManager),
}
