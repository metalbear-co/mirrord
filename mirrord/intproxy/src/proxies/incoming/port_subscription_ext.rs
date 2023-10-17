//! Utilities for handling toggleable `steal` feature in [`IncomingProxy`](super::IncomingProxy).

use mirrord_protocol::{
    tcp::{HttpResponseFallback, LayerTcp, LayerTcpSteal, StealType, TcpData},
    ClientMessage, ConnectionId, Port,
};

use super::InterceptorMessageOut;
use crate::protocol::PortSubscription;

fn get_port(steal_type: &StealType) -> Port {
    match steal_type {
        StealType::All(port) => *port,
        StealType::FilteredHttp(port, _) => *port,
        StealType::FilteredHttpEx(port, _) => *port,
    }
}

pub trait PortSubscriptionExt {
    fn port(&self) -> Port;

    /// Returns a correct port subscription request.
    fn wrap_agent_subscribe(&self) -> ClientMessage;

    /// Returns a port unsubscription request correct for this mode.
    fn wrap_agent_unsubscribe(&self) -> ClientMessage;

    /// Returns a connection unsubscription request correct for this mode.
    fn wrap_agent_unsubscribe_connection(&self, connection_id: ConnectionId) -> ClientMessage;

    /// Returns a message to be sent to the agent in response to data coming from an interceptor.
    fn wrap_response(
        &self,
        res: InterceptorMessageOut,
        connection_id: ConnectionId,
    ) -> Option<ClientMessage>;
}

impl PortSubscriptionExt for PortSubscription {
    fn port(&self) -> Port {
        match self {
            Self::Mirror(port) => *port,
            Self::Steal(steal_type) => get_port(steal_type),
        }
    }

    fn wrap_agent_subscribe(&self) -> ClientMessage {
        match self {
            Self::Mirror(port) => ClientMessage::Tcp(LayerTcp::PortSubscribe(*port)),
            Self::Steal(steal_type) => {
                ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(steal_type.clone()))
            }
        }
    }

    /// Returns a port unsubscription request correct for this mode.
    fn wrap_agent_unsubscribe(&self) -> ClientMessage {
        match self {
            Self::Mirror(port) => ClientMessage::Tcp(LayerTcp::PortUnsubscribe(*port)),
            Self::Steal(steal_type) => {
                ClientMessage::TcpSteal(LayerTcpSteal::PortUnsubscribe(get_port(steal_type)))
            }
        }
    }

    /// Returns a connection unsubscription request correct for this mode.
    fn wrap_agent_unsubscribe_connection(&self, connection_id: ConnectionId) -> ClientMessage {
        match self {
            Self::Mirror(..) => ClientMessage::Tcp(LayerTcp::ConnectionUnsubscribe(connection_id)),
            Self::Steal(..) => {
                ClientMessage::TcpSteal(LayerTcpSteal::ConnectionUnsubscribe(connection_id))
            }
        }
    }

    fn wrap_response(
        &self,
        res: InterceptorMessageOut,
        connection_id: ConnectionId,
    ) -> Option<ClientMessage> {
        match self {
            Self::Mirror(..) => None,
            Self::Steal(..) => match res {
                InterceptorMessageOut::Bytes(bytes) => {
                    Some(ClientMessage::TcpSteal(LayerTcpSteal::Data(TcpData {
                        connection_id,
                        bytes,
                    })))
                }
                InterceptorMessageOut::Http(HttpResponseFallback::Fallback(res)) => {
                    Some(ClientMessage::TcpSteal(LayerTcpSteal::HttpResponse(res)))
                }
                InterceptorMessageOut::Http(HttpResponseFallback::Framed(res)) => Some(
                    ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseFramed(res)),
                ),
            },
        }
    }
}
