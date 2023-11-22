use std::{
    collections::{hash_map::Entry, HashMap},
    net::SocketAddr,
};

use mirrord_intproxy_protocol::{
    IncomingResponse, LayerId, MessageId, PortSubscribe, PortUnsubscribe, ProxyToLayerMessage,
};
use mirrord_protocol::{ClientMessage, Port, RemoteResult, ResponseError};

use super::{port_subscription_ext::PortSubscriptionExt, IncomingProxyError};
use crate::{
    main_tasks::{ProxyMessage, ToLayer},
    remote_resources::RemoteResources,
};

struct Source {
    layer: LayerId,
    message: MessageId,
    request: PortSubscribe,
}

struct Subscription {
    queued_sources: Vec<Source>,
    active_source: Source,
    confirmed: bool,
}

impl Subscription {
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

    fn remove_source(mut self, listening_on: SocketAddr) -> Result<ClientMessage, Self> {
        let queue_size = self.queued_sources.len();
        self.queued_sources
            .retain(|source| source.request.listening_on != listening_on);
        if queue_size != self.queued_sources.len() {
            return Err(self);
        }

        if self.active_source.request.listening_on != listening_on {
            return Err(self);
        }

        match self.queued_sources.pop() {
            Some(next_in_queue) => {
                self.active_source = next_in_queue;
                Err(self)
            }
            None => Ok(self
                .active_source
                .request
                .subscription
                .wrap_agent_unsubscribe()),
        }
    }
}

#[derive(Default)]
pub struct SubscriptionsManager {
    remote_ports: RemoteResources<(Port, SocketAddr)>,
    subscriptions: HashMap<Port, Subscription>,
}

impl SubscriptionsManager {
    pub fn get(&self, port: Port) -> Option<&PortSubscribe> {
        self.subscriptions
            .get(&port)
            .map(|sub| &sub.active_source.request)
    }

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
            Ok(message) => Some(message),
            Err(subscription) => {
                self.subscriptions.insert(request.port, subscription);
                None
            }
        }
    }

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
            Err(err) => Err(IncomingProxyError::SubscriptionFailed(err)),
        }
    }

    pub fn layer_closed(&mut self, layer_id: LayerId) -> Vec<ClientMessage> {
        self.remote_ports
            .remove_all(layer_id)
            .filter_map(|(port, listening_on)| {
                let subscription = self.subscriptions.remove(&port)?;
                match subscription.remove_source(listening_on) {
                    Ok(message) => Some(message),
                    Err(subscription) => {
                        self.subscriptions.insert(port, subscription);
                        None
                    }
                }
            })
            .collect()
    }

    pub fn layer_forked(&mut self, parent: LayerId, child: LayerId) {
        self.remote_ports.clone_all(parent, child);
    }
}
