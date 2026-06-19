//! Each `*Proxy` (here [`OutgoingProxy`]) has its own `*ChaosEffect` (here
//! [`OutgoingChaosEffect`]), and this module has the `OutgoingProxy` implementation for `ChaosRule`
//! and how its effects affect the proxy operations.
use std::{
    ops::{ControlFlow, Not},
    sync::atomic::Ordering,
    time::Duration,
};

use mirrord_intproxy_protocol::{
    LayerId, MessageId, NetProtocol, OutgoingConnectRequest, OutgoingResponse, ProxyToLayerMessage,
};
use mirrord_protocol::{ClientMessage, ResponseError, outgoing::SocketAddress, uid::Uid};
use tokio::time::sleep;
use tracing::Level;

use crate::{
    background_tasks::MessageBus,
    main_tasks::ToLayer,
    proxies::outgoing::{InterceptorId, OutgoingProxy, net_protocol_ext::NetProtocolExt},
    session_monitor::chaos::rules::{ChaosEffectConnectionError, ChaosSelector, TcpChaosEffect},
};

#[derive(Debug)]
pub(super) enum ConnectinToWhat<'a> {
    Layer {
        to_layer: &'a ToLayer,
    },
    Agent {
        remote_address: SocketAddress,
        uid: Option<Uid>,
    },
}

/// The `ChaosRule` has different types for its effects, and they're inside the [`ChaosSelector`],
/// so we use this type here to simplify what the [`OutgoingProxy`] should do when a `ChaosRule` is
/// applied.
#[derive(Debug)]
pub enum OutgoingChaosEffect {
    /// `OutgoingProxy` should wait for the `wait_for` duration, before resuming the operation that
    /// matched the `ChaosRule`.
    LatencyToAgent {
        to_agent: ClientMessage,
        wait_for: Duration,
    },
    LatencyToLayer {
        to_layer: ToLayer,
        wait_for: Duration,
    },
    /// The connection should return an error to mirrord, and the error is already wrapped in a
    /// `ToLayer` message. The `OutgoingProxy` should wait for `after` before delivering resuming
    /// the operation that matched the `ChaosRule`.
    ConnectionError { to_layer: ToLayer, after: Duration },
}

impl OutgoingProxy {
    /// Applies `which_effect` [`OutgoingChaosEffect`] and returns if the [`OutgoingProxy`] should
    /// resume whatever it was going to do afterwards, or if it should skip its next action.
    ///
    /// # Returns
    ///
    /// [`ControlFlow::Break`] being returned here usually means that whatever `loop` (+ `select!`)
    /// combo should `continue`, skipping the rest of the operations that are usually performed. For
    /// example, say we're applying a [`OutgoingChaosEffect::ConnectionError`], we return
    /// `ControlFlow::Break` here, and in the place we call `apply_chaos_effect`, we turn this
    /// "break" into a `continue;`, so we don't return a connection error to the layer, and then try
    /// to process the connection request as normal (would make no sense).
    #[tracing::instrument(level = Level::INFO, skip(message_bus), ret)]
    async fn apply_chaos_effect(
        which_effect: OutgoingChaosEffect,
        message_bus: &mut MessageBus<OutgoingProxy>,
    ) -> ControlFlow<()> {
        match which_effect {
            OutgoingChaosEffect::LatencyToAgent { to_agent, wait_for } => {
                let agent_tx = message_bus.clone_agent_tx();

                tokio::spawn(async move {
                    sleep(wait_for).await;
                    agent_tx.send(to_agent).await;
                });

                ControlFlow::Break(())
            }
            OutgoingChaosEffect::LatencyToLayer { to_layer, wait_for } => {
                let layer_tx = message_bus.clone_layer_tx();

                tokio::spawn(async move {
                    sleep(wait_for).await;

                    let _ = layer_tx.send(to_layer.into()).await.inspect_err(|fail| {
                        tracing::warn!(?fail, "Failed sending message to layer!")
                    });
                });

                ControlFlow::Break(())
            }
            OutgoingChaosEffect::ConnectionError { to_layer, after } => {
                let layer_tx = message_bus.clone_layer_tx();
                tokio::spawn(async move {
                    sleep(after).await;

                    let _ = layer_tx.send(to_layer.into()).await.inspect_err(|fail| {
                        tracing::warn!(?fail, "Failed sending message to layer!")
                    });
                });

                ControlFlow::Break(())
            }
        }
    }

    /// Checks if the `ChaosRule` (stored in `self.chaos_rx`) applies to either of
    /// `remote_address`, `protocol`, or `hostname`.
    ///
    /// # Params
    ///
    /// - `to_effect`: Since the chaos effects are different types, to avoid matching on a bunch of
    ///   [`ChaosSelector`] s everywhere, this function gets passed with the relevant
    ///   `ChaosSelector` from the place where we should apply the `ChaosRule`. Translating this:
    ///   where we should check a `ChaosRule` that fails a connection request, we pass only the
    ///   relevant `ChaosSelector` to convert into the only relevant effect for a connection error,
    ///   which is [`OutgoingChaosEffect::ConnectionError`].
    ///
    /// # Returns
    ///
    /// If the rule matches, and we've rolled a hit, then we use `to_effect` to return the
    /// [`OutgoingChaosEffect`] that should be used.
    fn chaos_effect_for_address<T>(
        &self,
        remote_address: &SocketAddress,
        protocol: NetProtocol,
        hostname: Option<&String>,
        to_effect: impl FnOnce(&ChaosSelector) -> Option<T>,
    ) -> Option<T> {
        let rules = self.chaos_rx.borrow();

        let rule = rules
            .iter()
            .filter(|rule| rule.applies_to_address(remote_address, protocol, hostname))
            .max_by_key(|rule| rule.priority)?;

        if rule.selector_percentage().roll_for_hit().not() {
            return None;
        }

        let effect = to_effect(&rule.selector)?;

        rule.hit_count.fetch_add(1, Ordering::Relaxed);

        Some(effect)
    }

    /// Converts the [`ChaosEffectConnectionError`] into a [`OutgoingChaosEffect::ConnectionError`].
    fn connection_error_effect(
        effect: &ChaosEffectConnectionError,
        message_id: MessageId,
        layer_id: LayerId,
    ) -> OutgoingChaosEffect {
        OutgoingChaosEffect::ConnectionError {
            to_layer: ToLayer {
                message_id,
                layer_id,
                message: ProxyToLayerMessage::Outgoing(OutgoingResponse::Connect(Err(
                    ResponseError::RemoteIO(effect.error_type.clone().into()),
                ))),
            },
            after: effect.after,
        }
    }

    /// Helper function for `OutgoingRequest::Connect` related `ChaosRule`s.
    ///
    /// Exists mainly to help with the [`ChaosSelector`] matching.
    #[tracing::instrument(level = Level::INFO, skip(self, message_bus), ret)]
    pub(super) async fn chaos_effect_for_connect_error(
        &self,
        request: &OutgoingConnectRequest,
        message_id: MessageId,
        layer_id: LayerId,
        message_bus: &mut MessageBus<OutgoingProxy>,
    ) -> ControlFlow<()> {
        let which_effect = self.chaos_effect_for_address(
            &request.remote_address,
            request.protocol,
            request.hostname.as_ref(),
            |selector| match selector {
                ChaosSelector::Tcp {
                    effect: TcpChaosEffect::ConnectionError(effect),
                    ..
                } => Some(Self::connection_error_effect(effect, message_id, layer_id)),
                _ => None,
            },
        );

        match which_effect {
            Some(which_effect) => Self::apply_chaos_effect(which_effect, message_bus).await,
            None => ControlFlow::Continue(()),
        }
    }

    #[tracing::instrument(level = Level::INFO, skip(self, message_bus), ret)]
    pub(super) async fn chaos_effect_for_connect_latency(
        &self,
        request: &OutgoingConnectRequest,
        connectintowhat: ConnectinToWhat<'_>,
        message_bus: &mut MessageBus<OutgoingProxy>,
    ) -> ControlFlow<()> {
        let which_effect = self.chaos_effect_for_address(
            &request.remote_address,
            request.protocol,
            request.hostname.as_ref(),
            |selector| match selector {
                ChaosSelector::Tcp {
                    effect: TcpChaosEffect::Latency(effect),
                    ..
                } => match connectintowhat {
                    ConnectinToWhat::Layer { to_layer } => {
                        Some(OutgoingChaosEffect::LatencyToLayer {
                            to_layer: to_layer.clone(),
                            wait_for: effect.latency_duration(),
                        })
                    }
                    ConnectinToWhat::Agent {
                        remote_address,
                        uid,
                    } => {
                        let to_agent = request.protocol.wrap_agent_connect(remote_address, uid);

                        Some(OutgoingChaosEffect::LatencyToAgent {
                            to_agent,
                            wait_for: effect.latency_duration(),
                        })
                    }
                },
                _ => None,
            },
        );

        match which_effect {
            Some(which_effect) => Self::apply_chaos_effect(which_effect, message_bus).await,
            None => ControlFlow::Continue(()),
        }
    }

    /// Helper function for `TaskUpdate::Message` write messages, related to `ChaosRule`s.
    ///
    /// Exists mainly to help with the [`ChaosSelector`] matching.
    #[tracing::instrument(level = Level::INFO, skip(self), ret)]
    pub(super) fn chaos_latency_for_write(
        &self,
        interceptor_id: InterceptorId,
    ) -> Option<Duration> {
        let connection_info = self.connection_info(interceptor_id)?;

        self.chaos_effect_for_address(
            &connection_info.remote_address,
            interceptor_id.protocol,
            connection_info.hostname.as_ref(),
            |selector| match selector {
                ChaosSelector::Tcp {
                    effect: TcpChaosEffect::Latency(effect),
                    ..
                } => Some(effect.latency_duration()),
                _ => None,
            },
        )
    }
}
