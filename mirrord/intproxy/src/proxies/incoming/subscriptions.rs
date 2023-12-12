use std::{
    collections::{hash_map::Entry, HashMap},
    net::SocketAddr,
};

use mirrord_intproxy_protocol::{
    IncomingResponse, LayerId, MessageId, PortSubscribe, PortUnsubscribe, ProxyToLayerMessage,
};
use mirrord_protocol::{BlockedAction, ClientMessage, Port, RemoteResult, ResponseError};

use super::{port_subscription_ext::PortSubscriptionExt, IncomingProxyError};
use crate::{
    main_tasks::{ProxyMessage, ToLayer},
    remote_resources::RemoteResources,
};

/// Represents a source of subscription - a `listen` call in the layer.
#[derive(Debug)]
struct Source {
    layer: LayerId,
    message: MessageId,
    request: PortSubscribe,
}

/// Represents a port subscription in the agent.
#[derive(Debug)]
struct Subscription {
    /// Previous sources of this subscription. Each of these was at some point active, but was
    /// later overwritten.
    queued_sources: Vec<Source>,
    /// Latest source of this subscription.
    active_source: Source,
    /// Whether this subscription is confirmed.
    confirmed: bool,
}

impl Subscription {
    /// Creates a new subscription from the given [`Source`].
    /// Additionally returns a message to be sent to the agent.
    fn new(source: Source) -> (Self, ClientMessage) {
        let message = source.request.subscription.agent_subscribe();

        (
            Self {
                queued_sources: Default::default(),
                active_source: source,
                confirmed: false,
            },
            message,
        )
    }

    /// Overwrites the active subscription [`Source`].
    /// Returns a message to be sent to the layer.
    /// Returns [`None`] if this subscription is still waiting for confirmation.
    fn push_source(&mut self, source: Source) -> Option<ToLayer> {
        let message = if self.confirmed {
            // Agent already confirmed this subscription.
            Some(ToLayer {
                message_id: source.message,
                layer_id: source.layer,
                message: ProxyToLayerMessage::Incoming(IncomingResponse::PortSubscribe(Ok(()))),
            })
        } else {
            // Waiting for agent confirmation.
            None
        };

        let previous_source = std::mem::replace(&mut self.active_source, source);
        self.queued_sources.push(previous_source);

        message
    }

    /// Confirms this subscription.
    /// Returns messages to be sent to the layers.
    fn confirm(&mut self) -> Vec<ToLayer> {
        if self.confirmed {
            return vec![];
        }

        self.confirmed = true;

        self.queued_sources
            .iter()
            .chain(std::iter::once(&self.active_source))
            .map(|source| ToLayer {
                layer_id: source.layer,
                message_id: source.message,
                message: ProxyToLayerMessage::Incoming(IncomingResponse::PortSubscribe(Ok(()))),
            })
            .collect()
    }

    /// Rejects this subscription with the given `reason`.
    /// Returns messages to be sent to the layers.
    /// Returns [`Err`] if this subscription was already confirmed.
    fn reject(self, reason: ResponseError) -> Result<Vec<ToLayer>, Self> {
        if self.confirmed {
            return Err(self);
        }

        let responses = self
            .queued_sources
            .into_iter()
            .chain(std::iter::once(self.active_source))
            .map(|source| ToLayer {
                layer_id: source.layer,
                message_id: source.message,
                message: ProxyToLayerMessage::Incoming(IncomingResponse::PortSubscribe(Err(
                    reason.clone(),
                ))),
            })
            .collect();

        Ok(responses)
    }

    /// Removed a source from this subscription.
    /// If this source is the last one, returns [`Err`] with a message to be sent to the agent.
    fn remove_source(mut self, listening_on: SocketAddr) -> Result<Self, ClientMessage> {
        let queue_size = self.queued_sources.len();
        self.queued_sources
            .retain(|source| source.request.listening_on != listening_on);
        if queue_size != self.queued_sources.len() {
            return Ok(self);
        }

        if self.active_source.request.listening_on != listening_on {
            return Ok(self);
        }

        match self.queued_sources.pop() {
            Some(next_in_queue) => {
                self.active_source = next_in_queue;
                Ok(self)
            }
            None => Err(self
                .active_source
                .request
                .subscription
                .wrap_agent_unsubscribe()),
        }
    }
}

/// Manages port subscriptions across all connected layers.
/// Logic of this struct is a bit complicated for several reasons:
/// 1. Layer can subscribe to a single port multiple times (e.g. with `port_mapping`)
/// 2. Layer can fork, thus duplicating the OS listener socket related to a subscription
/// 3. Layer can be connected to multiple agents, each of which idependently responds to
///    subscription requests
/// 4. Subscription response from the agent most of the time cannot be tracked down to its
///    subscription request
#[derive(Default)]
pub struct SubscriptionsManager {
    remote_ports: RemoteResources<(Port, SocketAddr)>,
    subscriptions: HashMap<Port, Subscription>,
}

impl SubscriptionsManager {
    /// Returns active [`PortSubscribe`] request for the given [`Port`].
    pub fn get(&self, port: Port) -> Option<&PortSubscribe> {
        self.subscriptions
            .get(&port)
            .map(|sub| &sub.active_source.request)
    }

    /// Registers a new port subscription in this struct.
    /// Optionally returns a message to be sent.
    ///
    /// Subsequent subscriptions of the same port will take precedence over previous ones, meaning
    /// that new connections will be routed to the listener from the most recent [`PortSubscribe`]
    /// request.
    pub fn layer_subscribed(
        &mut self,
        layer_id: LayerId,
        message_id: MessageId,
        request: PortSubscribe,
    ) -> Option<ProxyMessage> {
        self.remote_ports.add(
            layer_id,
            (request.subscription.port(), request.listening_on),
        );

        let port = request.subscription.port();
        let source = Source {
            layer: layer_id,
            message: message_id,
            request,
        };

        match self.subscriptions.entry(port) {
            Entry::Occupied(mut e) => e.get_mut().push_source(source).map(ProxyMessage::ToLayer),
            Entry::Vacant(e) => {
                let (subscription, message) = Subscription::new(source);
                e.insert(subscription);
                Some(ProxyMessage::ToAgent(message))
            }
        }
    }

    /// Unregisters a subscription from this struct.
    /// Optionally returns a message to be sent to the agent.
    pub fn layer_unsubscribed(
        &mut self,
        layer_id: LayerId,
        request: PortUnsubscribe,
    ) -> Option<ClientMessage> {
        let closed_in_all_forks = self
            .remote_ports
            .remove(layer_id, (request.port, request.listening_on));
        if !closed_in_all_forks {
            return None;
        }

        let Some(subscription) = self.subscriptions.remove(&request.port) else {
            return None;
        };

        match subscription.remove_source(request.listening_on) {
            Ok(subscription) => {
                self.subscriptions.insert(request.port, subscription);
                None
            }
            Err(message) => Some(message),
        }
    }

    /// Notifies this struct about agent's response.
    /// Returns messages to be sent to the layers.
    #[tracing::instrument(level = "trace", ret, skip(self))]
    pub fn agent_responded(
        &mut self,
        result: RemoteResult<Port>,
    ) -> Result<Vec<ToLayer>, IncomingProxyError> {
        match result {
            Ok(port) => {
                let Some(subscription) = self.subscriptions.get_mut(&port) else {
                    return Ok(vec![]);
                };

                Ok(subscription.confirm())
            }
            Err(ResponseError::PortAlreadyStolen(port)) => {
                let Some(subscription) = self.subscriptions.remove(&port) else {
                    return Ok(vec![]);
                };

                match subscription.reject(ResponseError::PortAlreadyStolen(port)) {
                    Ok(responses) => Ok(responses),
                    Err(subscription) => {
                        self.subscriptions.insert(port, subscription);
                        Ok(vec![])
                    }
                }
            }
            Err(
                ref response_err @ ResponseError::Forbidden {
                    blocked_action: BlockedAction::Steal(ref steal_type),
                    ..
                },
            ) => {
                tracing::warn!("Port subscribe blocked by policy: {response_err}");
                let Some(subscription) = self.subscriptions.remove(&steal_type.get_port()) else {
                    return Ok(vec![]);
                };
                subscription
                    .reject(response_err.clone())
                    .map_err(|sub|{
                        tracing::error!("Subscription {sub:?} was confirmed before, then requested again and blocked by a policy.");
                        IncomingProxyError::SubscriptionFailed(response_err.clone())
                    })
            }
            Err(err) => Err(IncomingProxyError::SubscriptionFailed(err)),
        }
    }

    /// Notifies this struct about layer closing.
    /// Returns messages to be sent to the agent.
    pub fn layer_closed(&mut self, layer_id: LayerId) -> Vec<ClientMessage> {
        self.remote_ports
            .remove_all(layer_id)
            .filter_map(|(port, listening_on)| {
                let subscription = self.subscriptions.remove(&port)?;
                match subscription.remove_source(listening_on) {
                    Ok(subscription) => {
                        self.subscriptions.insert(port, subscription);
                        None
                    }
                    Err(message) => Some(message),
                }
            })
            .collect()
    }

    /// Notifies this struct about layer forking.
    pub fn layer_forked(&mut self, parent: LayerId, child: LayerId) {
        self.remote_ports.clone_all(parent, child);
    }
}

#[cfg(test)]
mod test {
    use mirrord_intproxy_protocol::PortSubscription;
    use mirrord_protocol::tcp::LayerTcp;

    use super::*;

    #[test]
    fn with_double_subscribe() {
        let listener_1 = "127.0.0.1:1111".parse().unwrap();
        let listener_2 = "127.0.0.1:2222".parse().unwrap();

        let mut manager = SubscriptionsManager::default();

        let response = manager.layer_subscribed(
            LayerId(0),
            0,
            PortSubscribe {
                listening_on: listener_1,
                subscription: PortSubscription::Mirror(80),
            },
        );
        assert!(
            matches!(
                response,
                Some(ProxyMessage::ToAgent(ClientMessage::Tcp(
                    LayerTcp::PortSubscribe(80)
                )))
            ),
            "{response:?}"
        );

        let response = manager.layer_subscribed(
            LayerId(0),
            1,
            PortSubscribe {
                listening_on: listener_2,
                subscription: PortSubscription::Mirror(80),
            },
        );
        assert!(response.is_none(), "{response:?}");

        let mut responses = manager.agent_responded(Ok(80)).unwrap();
        assert_eq!(responses.len(), 2, "{responses:?}");
        responses.sort_by_key(|r| r.message_id);
        for i in [0, 1] {
            let response = responses.get(i).unwrap();
            assert!(
                matches!(
                    response,
                    ToLayer {
                        layer_id: LayerId(0),
                        message: ProxyToLayerMessage::Incoming(IncomingResponse::PortSubscribe(
                            Ok(())
                        )),
                        message_id,
                    } if *message_id == i as u64
                ),
                "{i}: {response:?}"
            );
        }
        assert_eq!(manager.get(80).unwrap().listening_on, listener_2);

        let response = manager.layer_unsubscribed(
            LayerId(0),
            PortUnsubscribe {
                port: 80,
                listening_on: listener_2,
            },
        );
        assert!(response.is_none(), "{response:?}");
        assert_eq!(manager.get(80).unwrap().listening_on, listener_1);

        let response = manager.layer_unsubscribed(
            LayerId(0),
            PortUnsubscribe {
                port: 80,
                listening_on: listener_1,
            },
        );
        assert!(matches!(
            response,
            Some(ClientMessage::Tcp(LayerTcp::PortUnsubscribe(80)))
        ));
        assert!(manager.get(80).is_none());
    }

    #[test]
    fn with_fork() {
        let listening_on = "127.0.0.1:1111".parse().unwrap();

        let mut manager = SubscriptionsManager::default();

        let response = manager.layer_subscribed(
            LayerId(0),
            0,
            PortSubscribe {
                listening_on,
                subscription: PortSubscription::Mirror(80),
            },
        );
        assert!(
            matches!(
                response,
                Some(ProxyMessage::ToAgent(ClientMessage::Tcp(
                    LayerTcp::PortSubscribe(80)
                )))
            ),
            "{response:?}"
        );

        let responses = manager.agent_responded(Ok(80)).unwrap();
        assert_eq!(responses.len(), 1, "{responses:?}");
        let response = responses.into_iter().next().unwrap();
        assert!(
            matches!(
                response,
                ToLayer {
                    layer_id: LayerId(0),
                    message: ProxyToLayerMessage::Incoming(IncomingResponse::PortSubscribe(Ok(()))),
                    message_id: 0,
                }
            ),
            "{response:?}"
        );
        assert_eq!(manager.get(80).unwrap().listening_on, listening_on);

        manager.layer_forked(LayerId(0), LayerId(1));

        let response = manager.layer_unsubscribed(
            LayerId(0),
            PortUnsubscribe {
                port: 80,
                listening_on,
            },
        );
        assert!(response.is_none(), "{response:?}");
        assert_eq!(manager.get(80).unwrap().listening_on, listening_on);

        let response = manager.layer_unsubscribed(
            LayerId(1),
            PortUnsubscribe {
                port: 80,
                listening_on,
            },
        );
        assert!(matches!(
            response,
            Some(ClientMessage::Tcp(LayerTcp::PortUnsubscribe(80)))
        ));
        assert!(manager.get(80).is_none());
    }

    #[test]
    fn with_double_response() {
        let listening_on = "127.0.0.1:1111".parse().unwrap();

        let mut manager = SubscriptionsManager::default();

        let response = manager.layer_subscribed(
            LayerId(0),
            0,
            PortSubscribe {
                listening_on,
                subscription: PortSubscription::Mirror(80),
            },
        );
        assert!(
            matches!(
                response,
                Some(ProxyMessage::ToAgent(ClientMessage::Tcp(
                    LayerTcp::PortSubscribe(80)
                )))
            ),
            "{response:?}"
        );

        let responses = manager.agent_responded(Ok(80)).unwrap();
        assert_eq!(responses.len(), 1, "{responses:?}");
        let response = responses.into_iter().next().unwrap();
        assert!(
            matches!(
                response,
                ToLayer {
                    layer_id: LayerId(0),
                    message: ProxyToLayerMessage::Incoming(IncomingResponse::PortSubscribe(Ok(()))),
                    message_id: 0,
                }
            ),
            "{response:?}"
        );
        assert_eq!(manager.get(80).unwrap().listening_on, listening_on);

        let responses = manager
            .agent_responded(Err(ResponseError::PortAlreadyStolen(80)))
            .unwrap();
        assert!(responses.is_empty(), "{responses:?}");
    }
}
