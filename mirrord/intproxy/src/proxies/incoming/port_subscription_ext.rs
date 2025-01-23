//! Utilities for handling toggleable `steal` feature in [`IncomingProxy`](super::IncomingProxy).

use mirrord_intproxy_protocol::PortSubscription;
use mirrord_protocol::{
    tcp::{LayerTcp, LayerTcpSteal, StealType},
    ClientMessage, Port,
};

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
}
