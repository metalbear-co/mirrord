//! Utilities for handling toggleable `steal` feature in [`IncomingProxy`](super::IncomingProxy).

use mirrord_intproxy_protocol::PortSubscription;
use mirrord_protocol::{
    tcp::{HttpResponseFallback, LayerTcp, LayerTcpSteal, StealType, TcpData},
    ClientMessage, ConnectionId, Port,
};

use super::interceptor::MessageOut;

/// Retrieves subscribed port from the given [`StealType`].
fn get_port(steal_type: &StealType) -> Port {
    match steal_type {
        StealType::All(port) => *port,
        StealType::FilteredHttp(port, _) => *port,
        StealType::FilteredHttpEx(port, _) => *port,
    }
}

/// Trait for [`PortSubscription`] that handles differences in [`mirrord_protocol::tcp`] between the
/// `steal` and the `mirror` flow. Allows to unify logic for both flows.
pub trait PortSubscriptionExt {
    /// Returns the subscribed port.
    fn port(&self) -> Port;

    /// Returns a subscribe request to be sent to the agent.
    fn agent_subscribe(&self) -> ClientMessage;

    /// Returns an unsubscribe request to be sent to the agent.
    fn wrap_agent_unsubscribe(&self) -> ClientMessage;

    /// Returns an unsubscribe connection request to be sent to the agent.
    fn wrap_agent_unsubscribe_connection(&self, connection_id: ConnectionId) -> ClientMessage;

    /// Returns a message to be sent to the agent in response to data coming from an interceptor.
    /// [`None`] means that the data should be discarded.
    fn wrap_response(&self, res: MessageOut, connection_id: ConnectionId) -> Option<ClientMessage>;
}

impl PortSubscriptionExt for PortSubscription {
    fn port(&self) -> Port {
        match self {
            Self::Mirror(port) => *port,
            Self::Steal(steal_type) => get_port(steal_type),
        }
    }

    /// [`LayerTcp::PortSubscribe`] or [`LayerTcpSteal::PortSubscribe`].
    fn agent_subscribe(&self) -> ClientMessage {
        match self {
            Self::Mirror(port) => ClientMessage::Tcp(LayerTcp::PortSubscribe(*port)),
            Self::Steal(steal_type) => {
                ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(steal_type.clone()))
            }
        }
    }

    /// [`LayerTcp::PortUnsubscribe`] or [`LayerTcpSteal::PortUnsubscribe`].
    fn wrap_agent_unsubscribe(&self) -> ClientMessage {
        match self {
            Self::Mirror(port) => ClientMessage::Tcp(LayerTcp::PortUnsubscribe(*port)),
            Self::Steal(steal_type) => {
                ClientMessage::TcpSteal(LayerTcpSteal::PortUnsubscribe(get_port(steal_type)))
            }
        }
    }

    /// [`LayerTcp::ConnectionUnsubscribe`] or [`LayerTcpSteal::ConnectionUnsubscribe`].
    fn wrap_agent_unsubscribe_connection(&self, connection_id: ConnectionId) -> ClientMessage {
        match self {
            Self::Mirror(..) => ClientMessage::Tcp(LayerTcp::ConnectionUnsubscribe(connection_id)),
            Self::Steal(..) => {
                ClientMessage::TcpSteal(LayerTcpSteal::ConnectionUnsubscribe(connection_id))
            }
        }
    }

    /// Always [`None`] for the `mirror` mode - data coming from the layer is discarded.
    /// Corrent [`LayerTcpSteal`] variant for the `steal` mode.
    fn wrap_response(&self, res: MessageOut, connection_id: ConnectionId) -> Option<ClientMessage> {
        match self {
            Self::Mirror(..) => None,
            Self::Steal(..) => match res {
                MessageOut::Raw(bytes) => {
                    Some(ClientMessage::TcpSteal(LayerTcpSteal::Data(TcpData {
                        connection_id,
                        bytes,
                    })))
                }
                MessageOut::Http(HttpResponseFallback::Fallback(res)) => {
                    Some(ClientMessage::TcpSteal(LayerTcpSteal::HttpResponse(res)))
                }
                MessageOut::Http(HttpResponseFallback::Framed(res)) => Some(
                    ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseFramed(res)),
                ),
            },
        }
    }
}
