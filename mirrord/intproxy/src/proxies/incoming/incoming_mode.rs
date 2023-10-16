//! Utilities for handling toggleable `steal` feature in [`IncomingProxy`](super::IncomingProxy).

use mirrord_protocol::{
    tcp::{HttpResponse, InternalHttpBody, LayerTcp, LayerTcpSteal, StealType, TcpData},
    ClientMessage, ConnectionId, Port,
};

use crate::protocol::{IncomingMode, StealHttpFilter, StealHttpSettings};

impl StealHttpSettings {
    /// Returns [`StealType`] to be used with the given port.
    fn steal_type(&self, port: Port) -> StealType {
        if !self.ports.contains(&port) {
            return StealType::All(port);
        }

        match &self.filter {
            StealHttpFilter::None => StealType::All(port),
            StealHttpFilter::HeaderDeprecated(filter) => {
                StealType::FilteredHttp(port, filter.clone())
            }
            StealHttpFilter::Filter(filter) => StealType::FilteredHttpEx(port, filter.clone()),
        }
    }
}

impl IncomingMode {
    /// Returns whether the `steal` feature is enabled.
    pub fn is_steal(&self) -> bool {
        matches!(self, Self::Steal(..))
    }

    /// Returns a port subscription request correct for this mode.
    pub fn wrap_agent_subscribe(&self, port: Port) -> ClientMessage {
        match self {
            Self::Mirror => ClientMessage::Tcp(LayerTcp::PortSubscribe(port)),
            Self::Steal(settings) => {
                ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(settings.steal_type(port)))
            }
        }
    }

    /// Returns a port unsubscription request correct for this mode.
    pub fn wrap_agent_unsubscribe(&self, port: Port) -> ClientMessage {
        match self {
            Self::Mirror => ClientMessage::Tcp(LayerTcp::PortUnsubscribe(port)),
            Self::Steal(..) => ClientMessage::TcpSteal(LayerTcpSteal::PortUnsubscribe(port)),
        }
    }

    /// Returns a connection unsubscription request correct for this mode.
    pub fn wrap_agent_unsubscribe_connection(&self, connection_id: ConnectionId) -> ClientMessage {
        match self {
            Self::Mirror => ClientMessage::Tcp(LayerTcp::ConnectionUnsubscribe(connection_id)),
            Self::Steal(..) => {
                ClientMessage::TcpSteal(LayerTcpSteal::ConnectionUnsubscribe(connection_id))
            }
        }
    }

    pub fn wrap_raw_response(&self, data: TcpData) -> Option<ClientMessage> {
        match self {
            Self::Mirror => None,
            Self::Steal(..) => Some(ClientMessage::TcpSteal(LayerTcpSteal::Data(data))),
        }
    }

    pub fn wrap_http_response(&self, res: HttpResponse<Vec<u8>>) -> Option<ClientMessage> {
        match self {
            Self::Mirror => None,
            Self::Steal(..) => Some(ClientMessage::TcpSteal(LayerTcpSteal::HttpResponse(res))),
        }
    }

    pub fn wrap_http_response_framed(
        &self,
        res: HttpResponse<InternalHttpBody>,
    ) -> Option<ClientMessage> {
        match self {
            Self::Mirror => None,
            Self::Steal(..) => Some(ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseFramed(
                res,
            ))),
        }
    }
}
