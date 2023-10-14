use std::fmt;

use mirrord_protocol::{ClientMessage, DaemonMessage};
use tokio::net::TcpStream;

use crate::protocol::{LayerId, LayerToProxyMessage, MessageId, ProxyToLayerMessage};

/// Messages sent back to the [`IntProxy`](crate::IntProxy) from the main background tasks. See
/// [`MainTaskId`](crate::).
#[derive(Debug)]
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
}

#[derive(Debug)]
pub struct ToLayer {
    pub message_id: MessageId,
    pub layer_id: LayerId,
    pub message: ProxyToLayerMessage,
}

#[derive(Debug)]
pub struct FromLayer {
    pub message_id: MessageId,
    pub layer_id: LayerId,
    pub message: LayerToProxyMessage,
}

#[derive(Debug)]
pub struct NewLayer {
    pub stream: TcpStream,
    pub id: LayerId,
    pub parent_id: Option<LayerId>,
}

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
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LayerForked {
    pub child: LayerId,
    pub parent: LayerId,
}

#[derive(Debug, Clone, Copy)]
pub struct LayerClosed {
    pub id: LayerId,
}
