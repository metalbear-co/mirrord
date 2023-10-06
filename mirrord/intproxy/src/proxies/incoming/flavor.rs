use std::collections::HashSet;

use mirrord_config::LayerConfig;
use mirrord_protocol::{
    tcp::{Filter, HttpFilter, LayerTcp, LayerTcpSteal, StealType},
    ClientMessage, ConnectionId, Port,
};
use thiserror::Error;

pub enum StealHttpFilter {
    None,
    HeaderDeprecated(Filter),
    Filter(HttpFilter),
}

pub struct StealHttpSettings {
    /// The HTTP filter to use.
    pub filter: StealHttpFilter,
    /// Ports to filter HTTP on
    pub ports: HashSet<Port>,
}

impl StealHttpSettings {
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

pub enum Flavor {
    Mirror,
    Steal(StealHttpSettings),
}

impl Flavor {
    pub fn is_steal(&self) -> bool {
        matches!(self, Self::Steal(..))
    }

    pub fn wrap_agent_subscribe(&self, port: Port) -> ClientMessage {
        match self {
            Self::Mirror => ClientMessage::Tcp(LayerTcp::PortSubscribe(port)),
            Self::Steal(settings) => {
                ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(settings.steal_type(port)))
            }
        }
    }

    pub fn wrap_agent_unsubscribe(&self, port: Port) -> ClientMessage {
        match self {
            Self::Mirror => ClientMessage::Tcp(LayerTcp::PortUnsubscribe(port)),
            Self::Steal(..) => ClientMessage::TcpSteal(LayerTcpSteal::PortUnsubscribe(port)),
        }
    }

    pub fn wrap_agent_unsubscribe_connection(&self, connection_id: ConnectionId) -> ClientMessage {
        match self {
            Self::Mirror => ClientMessage::Tcp(LayerTcp::ConnectionUnsubscribe(connection_id)),
            Self::Steal(..) => {
                ClientMessage::TcpSteal(LayerTcpSteal::ConnectionUnsubscribe(connection_id))
            }
        }
    }

    pub fn new(config: &LayerConfig) -> Result<Self, IncomingFlavorError> {
        if !config.feature.network.incoming.is_steal() {
            return Ok(Self::Mirror);
        }

        let http_header_filter_config = &config.feature.network.incoming.http_header_filter;
        let http_filter_config = &config.feature.network.incoming.http_filter;

        let ports = {
            if http_header_filter_config.filter.is_some() {
                http_header_filter_config
                    .ports
                    .as_slice()
                    .iter()
                    .copied()
                    .collect()
            } else {
                http_filter_config
                    .ports
                    .as_slice()
                    .iter()
                    .copied()
                    .collect()
            }
        };

        let filter = match (
            &http_filter_config.path_filter,
            &http_filter_config.header_filter,
            &http_header_filter_config.filter,
        ) {
            (Some(path), None, None) => StealHttpFilter::Filter(HttpFilter::Path(
                Filter::new(path.into()).map_err(IncomingFlavorError::InvalidFilterError)?,
            )),
            (None, Some(header), None) => StealHttpFilter::Filter(HttpFilter::Header(
                Filter::new(header.into()).map_err(IncomingFlavorError::InvalidFilterError)?,
            )),
            (None, None, Some(header)) => StealHttpFilter::HeaderDeprecated(
                Filter::new(header.into()).map_err(IncomingFlavorError::InvalidFilterError)?,
            ),
            (None, None, None) => StealHttpFilter::None,
            _ => return Err(IncomingFlavorError::MultipleFiltersError),
        };

        Ok(Self::Steal(StealHttpSettings { filter, ports }))
    }
}

#[derive(Error, Debug)]
pub enum IncomingFlavorError {
    #[error("invalid filter expression: `{0}`")]
    InvalidFilterError(fancy_regex::Error),
    #[error("multiple HTTP filters specified")]
    MultipleFiltersError,
}
