//! Utilities for handling toggleable `steal` feature in [`IncomingProxy`](super::IncomingProxy).

use std::collections::HashSet;

use mirrord_config::LayerConfig;
use mirrord_protocol::{
    tcp::{Filter, HttpFilter, LayerTcp, LayerTcpSteal, StealType},
    ClientMessage, ConnectionId, Port,
};
use thiserror::Error;

/// HTTP filter used by the layer with the `steal` feature.
pub enum StealHttpFilter {
    /// No filter.
    None,
    /// Header filter, deprecated.
    HeaderDeprecated(Filter),
    /// More recent filter (header or path).
    Filter(HttpFilter),
}

/// Settings for handling HTTP with the `steal` feature.
pub struct StealHttpSettings {
    /// The HTTP filter to use.
    pub filter: StealHttpFilter,
    /// Ports to filter HTTP on.
    pub ports: HashSet<Port>,
}

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

/// Operation mode for the [`IncomingProxy`](super::IncomingProxy).
pub enum IncomingMode {
    /// The agent sends data to both the user application and the remote target.
    /// Data coming from the layer is discarded.
    Mirror,
    /// The agent sends data only to the user application.
    /// Data coming from the layer is sent to the agent.
    Steal(StealHttpSettings),
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

    /// Creates a new instance from the given [`LayerConfig`].
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

/// Errors that can occur when extracting [`IncomingMode`] from the [`LayerConfig`].
#[derive(Error, Debug)]
pub enum IncomingFlavorError {
    /// Failed to create a [`Filter`] from a regex.
    #[error("invalid filter expression: `{0}`")]
    InvalidFilterError(fancy_regex::Error),
    /// Multiple HTTP filters were specified.
    #[error("multiple HTTP filters specified")]
    MultipleFiltersError,
}
