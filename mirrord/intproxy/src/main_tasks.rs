use std::fmt;

use mirrord_intproxy_protocol::{LayerId, LayerToProxyMessage, MessageId, ProxyToLayerMessage};
use mirrord_protocol::{ClientMessage, DaemonMessage};
use tokio::net::TcpStream;

/// Messages sent back to the [`IntProxy`](crate::IntProxy) from the main background tasks. See
/// [`MainTaskId`].
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub enum ProxyMessage {
    /// Message to be sent to the agent.
    ToAgent(ClientMessage),
    /// Message to be sent to a layer instance.
    ToLayer(ToLayer),
    /// Message received from the agent.
    FromAgent(DaemonMessage),
    /// Message received from a layer instance.
    FromLayer(FromLayer),
    /// New layer instance to serve.
    NewLayer(NewLayer),
    /// Connection to agent was dropped and needs reload.
    ConnectionRefresh,
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
}

#[cfg(test)]
impl PartialEq for NewLayer {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.parent_id == other.parent_id
    }
}

#[cfg(test)]
impl Eq for NewLayer {}

impl From<ClientMessage> for ProxyMessage {
    fn from(value: ClientMessage) -> Self {
        Self::ToAgent(value)
    }
}

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
            Self::LayerConnection(id) => write!(f, "LAYER_CONNECTION {}", id.0),
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
