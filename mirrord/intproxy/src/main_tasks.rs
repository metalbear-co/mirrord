use std::fmt;

use mirrord_intproxy_protocol::{
    LayerId, LayerToProxyMessage, MessageId, ProcessInfo, ProxyToLayerMessage,
};
use mirrord_protocol::{
    DaemonMessage, RemoteResult,
    dns::{DnsLookup, GetAddrInfoRequestV2},
};
use mirrord_protocol_io::{Client, TxHandle};
use tokio::net::TcpStream;

use crate::proxies::outgoing::dns::DnsQueryId;

/// Messages sent back to the [`IntProxy`](crate::IntProxy) from the main background tasks. See
/// [`MainTaskId`].
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
#[allow(clippy::large_enum_variant)] // the difference is not that big
pub enum ProxyMessage {
    /// Message to be sent to a layer instance.
    ToLayer(ToLayer),
    /// Message received from the agent.
    FromAgent(DaemonMessage),
    /// Message received from a layer instance.
    FromLayer(FromLayer),
    /// New layer instance to serve.
    NewLayer(NewLayer),
    /// Connection to agent was dropped and needs reload.
    ConnectionRefresh(ConnectionRefresh),
    /// A DNS query intercepted by the [`OutgoingProxy`](crate::proxies::outgoing::OutgoingProxy)
    /// needs remote resolution.
    DnsFilteringLookup(DnsFilteringLookup),
    /// Remote resolution of an intercepted DNS query finished.
    DnsFilteringLookupResult(DnsFilteringLookupResult),
}

/// Request to resolve a DNS query that the
/// [`OutgoingProxy`](crate::proxies::outgoing::OutgoingProxy) intercepted on its way to a DNS
/// server, routed through the [`SimpleProxy`](crate::proxies::simple::SimpleProxy) so that it
/// shares one queue with the layers' `getaddrinfo` calls.
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct DnsFilteringLookup {
    pub id: DnsQueryId,
    pub request: GetAddrInfoRequestV2,
}

/// Answer to a [`DnsFilteringLookup`].
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct DnsFilteringLookupResult {
    pub id: DnsQueryId,
    pub result: RemoteResult<DnsLookup>,
}

#[cfg(test)]
impl ProxyMessage {
    pub fn unwrap_proxy_to_layer_message(self) -> ProxyToLayerMessage {
        match self {
            Self::ToLayer(to_layer) => to_layer.message,
            other => panic!("expected proxy to layer message, found {other:?}"),
        }
    }
}

#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct ToLayer {
    pub message_id: MessageId,
    pub layer_id: LayerId,
    pub message: ProxyToLayerMessage,
}

#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct FromLayer {
    pub message_id: MessageId,
    pub layer_id: LayerId,
    pub message: LayerToProxyMessage,
}

#[derive(Debug)]
pub struct NewLayer {
    pub stream: TcpStream,
    pub id: LayerId,
    /// [`LayerId`] of the fork parent.
    pub parent_id: Option<LayerId>,
    /// Process information for the connecting layer.
    pub process_info: ProcessInfo,
}

#[cfg(test)]
impl PartialEq for NewLayer {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.parent_id == other.parent_id
    }
}

#[cfg(test)]
impl Eq for NewLayer {}

impl From<ToLayer> for ProxyMessage {
    fn from(value: ToLayer) -> Self {
        Self::ToLayer(value)
    }
}

impl From<DaemonMessage> for ProxyMessage {
    fn from(value: DaemonMessage) -> Self {
        Self::FromAgent(value)
    }
}

impl From<FromLayer> for ProxyMessage {
    fn from(value: FromLayer) -> Self {
        Self::FromLayer(value)
    }
}

impl From<NewLayer> for ProxyMessage {
    fn from(value: NewLayer) -> Self {
        Self::NewLayer(value)
    }
}

impl From<DnsFilteringLookup> for ProxyMessage {
    fn from(value: DnsFilteringLookup) -> Self {
        Self::DnsFilteringLookup(value)
    }
}

impl From<DnsFilteringLookupResult> for ProxyMessage {
    fn from(value: DnsFilteringLookupResult) -> Self {
        Self::DnsFilteringLookupResult(value)
    }
}

/// Enumerated ids of main [`BackgroundTask`](crate::background_tasks::BackgroundTask)s used by
/// [`IntProxy`](crate::IntProxy).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MainTaskId {
    LayerInitializer,
    SimpleProxy,
    OutgoingProxy,
    IncomingProxy,
    PingPong,
    AgentConnection,
    FilesProxy,
    LayerConnection(LayerId),
}

impl fmt::Display for MainTaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LayerInitializer => f.write_str("LAYER_INITIALIZER"),
            Self::SimpleProxy => f.write_str("SIMPLE_PROXY"),
            Self::OutgoingProxy => f.write_str("OUTGOING_PROXY"),
            Self::PingPong => f.write_str("PING_PONG"),
            Self::AgentConnection => f.write_str("AGENT_CONNECTION"),
            Self::LayerConnection(id) => write!(f, "LAYER_CONNECTION_{}", id.0),
            Self::IncomingProxy => f.write_str("INCOMING_PROXY"),
            Self::FilesProxy => f.write_str("FILES_PROXY"),
        }
    }
}

/// Notification about layer for. Useful to some background tasks.
#[derive(Debug, Clone, Copy)]
pub struct LayerForked {
    pub child: LayerId,
    pub parent: LayerId,
}

/// Notification about layer for. Useful to some background tasks.
#[derive(Debug, Clone, Copy)]
pub struct LayerClosed {
    pub id: LayerId,
}

/// Notification about start and end of reconnection to agent.
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub enum ConnectionRefresh {
    Start,
    End(TxHandle<Client>),
    Request,
}

impl ConnectionRefresh {
    /// Clone this object with a *FRESH* [`TxHandle`] created with
    /// [`TxHandle::another`]. Clones created with this method are
    /// appropriate to send to distinct background tasks.
    pub fn clone_with_another_handle(&self) -> Self {
        match self {
            Self::Start => Self::Start,
            Self::End(tx) => Self::End(tx.another()),
            Self::Request => Self::Request,
        }
    }
}
