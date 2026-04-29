//! Utilities for handling toggleable `steal` feature in [`IncomingProxy`](super::IncomingProxy).

use mirrord_intproxy_protocol::PortSubscription;
use mirrord_protocol::{
    ClientMessage, Port,
    tcp::{
        LayerTcp, LayerTcpSteal, MIRROR_HTTP_FILTER_VERSION, MirrorType, STEAL_RAW_TCP_VERSION,
        StealType,
    },
};

/// Retrieves subscribed port from the given [`StealType`].
fn get_port(steal_type: &StealType) -> Port {
    steal_type.get_port()
}

/// Trait for [`PortSubscription`] that handles differences in [`mirrord_protocol::tcp`] between the
/// `steal` and the `mirror` flow. Allows to unify logic for both flows.
pub trait PortSubscriptionExt {
    /// Returns the subscribed port.
    fn port(&self) -> Port;

    /// Returns whether this subscription requests raw TCP mode.
    fn requests_raw_tcp(&self) -> bool;

    /// Returns a subscribe request to be sent to the agent.
    fn agent_subscribe(&self, protocol_version: Option<&semver::Version>) -> ClientMessage;

    /// Returns an unsubscribe request to be sent to the agent.
    fn wrap_agent_unsubscribe(&self) -> ClientMessage;
}

impl PortSubscriptionExt for PortSubscription {
    fn port(&self) -> Port {
        match self {
            Self::Mirror(mirror_type) => mirror_type.get_port(),
            Self::Steal(steal_type) => get_port(steal_type),
        }
    }

    fn requests_raw_tcp(&self) -> bool {
        matches!(self, Self::Steal(StealType::AllRawTcp(..)))
    }

    /// [`LayerTcp::PortSubscribe`], [`LayerTcp::PortSubscribeFilteredHttp`], or
    /// [`LayerTcpSteal::PortSubscribe`].
    fn agent_subscribe(&self, protocol_version: Option<&semver::Version>) -> ClientMessage {
        match self {
            Self::Mirror(mirror_type) => match mirror_type {
                MirrorType::FilteredHttp(port, filter) => {
                    // Check if the agent supports filtered HTTP mirroring
                    if protocol_version
                        .is_some_and(|version| MIRROR_HTTP_FILTER_VERSION.matches(version))
                    {
                        ClientMessage::Tcp(LayerTcp::PortSubscribeFilteredHttp(
                            *port,
                            filter.clone(),
                        ))
                    } else {
                        // For older agents or when protocol version is unknown, fall back to
                        // regular mirroring without filter
                        tracing::warn!(
                            ?protocol_version,
                            "Negotiated mirrord-protocol version does not allow for using an HTTP filter when mirroring incoming traffic. \
                            The filter will be ignored."
                        );
                        ClientMessage::Tcp(LayerTcp::PortSubscribe(*port))
                    }
                }
                MirrorType::All(_) => {
                    ClientMessage::Tcp(LayerTcp::PortSubscribe(mirror_type.get_port()))
                }
            },
            Self::Steal(steal_type) => {
                // AllRawTcp requires a minimum agent protocol version. Fall back to All for
                // older or not-yet-negotiated agents to preserve backward compatibility.
                let effective = match steal_type {
                    StealType::AllRawTcp(port) => {
                        if protocol_version.is_some_and(|v| STEAL_RAW_TCP_VERSION.matches(v)) {
                            steal_type.clone()
                        } else {
                            if protocol_version.is_some() {
                                tracing::warn!(
                                    ?protocol_version,
                                    port,
                                    "Agent protocol version does not support StealType::AllRawTcp. \
                                     Falling back to StealType::All; HTTP detection will still run \
                                     on this port.",
                                );
                            } else {
                                tracing::debug!(
                                    port,
                                    "Agent protocol version unknown, downgrading AllRawTcp to All",
                                );
                            }
                            StealType::All(*port)
                        }
                    }
                    other => other.clone(),
                };
                ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(effective))
            }
        }
    }

    /// [`LayerTcp::PortUnsubscribe`] or [`LayerTcpSteal::PortUnsubscribe`].
    fn wrap_agent_unsubscribe(&self) -> ClientMessage {
        match self {
            Self::Mirror(mirror_type) => {
                ClientMessage::Tcp(LayerTcp::PortUnsubscribe(mirror_type.get_port()))
            }
            Self::Steal(steal_type) => {
                ClientMessage::TcpSteal(LayerTcpSteal::PortUnsubscribe(get_port(steal_type)))
            }
        }
    }
}
