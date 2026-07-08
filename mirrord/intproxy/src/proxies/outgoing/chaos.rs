//! Each `*Proxy` (here [`OutgoingProxy`]) has its own implementation of the mirrord chaos feature,
//! on how the `ChaosRule`s affects the proxy messages.
use std::{
    ops::{ControlFlow, Not},
    sync::atomic::Ordering,
    time::Duration,
};

use mirrord_intproxy_protocol::{
    LayerId, MessageId, NetProtocol, OutgoingConnectRequest, OutgoingResponse, ProxyToLayerMessage,
};
use mirrord_protocol::{ResponseError, outgoing::SocketAddress};
use tokio::time::sleep;
use tracing::Level;

use crate::{
    background_tasks::MessageBus,
    main_tasks::ToLayer,
    proxies::outgoing::{DeferredConnection, InterceptorId, OutgoingProxy, OutgoingProxyMessage},
    session_monitor::chaos::rules::{
        ChaosEffectConnectionError, ChaosSelector, ConnectionErrorType, TcpChaosEffect,
    },
};

/// Helper type for distinguishing between `ChaosEffectLatency.read` and `write` directions.
#[derive(Debug, Clone, Copy)]
enum ChaosLatencyDirection {
    Read,
    Write,
}

impl OutgoingProxy {
    /// Checks if the `ChaosRule` (stored in `self.chaos_rx`) applies to either of
    /// `remote_address`, `protocol`, or `hostname`.
    ///
    /// # Params
    ///
    /// - `to_effect`: The actual effect that we want to apply when the [`ChaosSelector`] was
    ///   triggered. Translating: if we should apply a delay to a `ConnectionRequest`, then
    ///   `to_effect` returns the `Duration` of this delay.
    ///
    /// # Returns
    ///
    /// If the rule matches, and we've rolled a hit, then we use `to_effect` to return the
    /// actual effect for this operation (i.e. a `Duration` if it's a delay).
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

    /// Converts the [`ChaosEffectConnectionError`] into the layer error response and delay.
    fn connection_error_effect(
        effect: &ChaosEffectConnectionError,
        message_id: MessageId,
        layer_id: LayerId,
    ) -> (ToLayer, Duration) {
        (
            ToLayer {
                message_id,
                layer_id,
                message: ProxyToLayerMessage::Outgoing(OutgoingResponse::Connect(Err(
                    ResponseError::RemoteIO(effect.error_type.clone().into()),
                ))),
            },
            effect.after,
        )
    }

    /// Helper function for `OutgoingRequest::Connect` related `ChaosRule`s.
    ///
    /// Exists mainly to help with the [`ChaosSelector`] matching.
    #[tracing::instrument(level = Level::DEBUG, skip(self, message_bus), ret)]
    pub(super) async fn chaos_effect_for_connect_error(
        &self,
        request: &OutgoingConnectRequest,
        message_id: MessageId,
        layer_id: LayerId,
        message_bus: &mut MessageBus<OutgoingProxy>,
    ) -> ControlFlow<()> {
        let effect = self.chaos_effect_for_address(
            &request.remote_address,
            request.protocol,
            request.hostname(),
            |selector| match selector {
                ChaosSelector::Tcp {
                    effect: TcpChaosEffect::ConnectionError(effect),
                    ..
                } => Some(Self::connection_error_effect(effect, message_id, layer_id)),
                _ => None,
            },
        );

        match effect {
            Some((to_layer, after)) => {
                if after.is_zero() {
                    message_bus.send(to_layer).await;
                } else {
                    let layer_tx = message_bus.clone_layer_tx();
                    tokio::spawn(async move {
                        sleep(after).await;

                        let _ = layer_tx.send(to_layer.into()).await.inspect_err(|fail| {
                            tracing::warn!(?fail, "Failed sending message to layer!")
                        });
                    });
                }

                ControlFlow::Break(())
            }
            None => ControlFlow::Continue(()),
        }
    }

    #[tracing::instrument(level = Level::DEBUG, skip(self, message_bus), ret)]
    pub(super) async fn chaos_effect_for_connect_latency(
        &self,
        request: OutgoingConnectRequest,
        message_id: MessageId,
        layer_id: LayerId,
        message_bus: &mut MessageBus<OutgoingProxy>,
    ) -> ControlFlow<(), OutgoingConnectRequest> {
        let effect = self.chaos_effect_for_address(
            &request.remote_address,
            request.protocol,
            request.hostname(),
            |selector| match selector {
                ChaosSelector::Tcp {
                    effect: TcpChaosEffect::Latency(effect),
                    ..
                } => effect.connection_latency_duration(),
                _ => None,
            },
        );

        match effect {
            Some(wait_for) if let Some(outgoing_tx) = message_bus.clone_self_tx() => {
                let deferred = DeferredConnection {
                    request,
                    message_id,
                    layer_id,
                };

                tokio::spawn(async move {
                    sleep(wait_for).await;

                    let _ = outgoing_tx
                        .send(OutgoingProxyMessage::DeferredConnect(deferred))
                        .await
                        .inspect_err(|fail| {
                            tracing::warn!(?fail, "Failed sending deferred connect!")
                        });
                });

                ControlFlow::Break(())
            }
            Some(_) => {
                tracing::debug!("Connection should be delayed, but outgoing_tx is closed.");
                ControlFlow::Continue(request)
            }
            None => ControlFlow::Continue(request),
        }
    }

    /// Latency to apply to an intercepted connection's data messages, if a `ChaosRule` with a
    /// *read* latency effect matches this connection.
    pub(super) fn chaos_read_latency_for_connection(
        &self,
        interceptor_id: InterceptorId,
    ) -> Option<Duration> {
        self.chaos_latency_for_connection(interceptor_id, ChaosLatencyDirection::Read)
    }

    /// Latency to apply to an intercepted connection's data messages, if a `ChaosRule` with a
    /// *write* latency effect matches this connection.
    pub(super) fn chaos_write_latency_for_connection(
        &self,
        interceptor_id: InterceptorId,
    ) -> Option<Duration> {
        self.chaos_latency_for_connection(interceptor_id, ChaosLatencyDirection::Write)
    }

    /// Call [`chaos_read_latency_for_connection()`] or [`chaos_write_latency_for_connection()`]
    /// instead.
    ///
    /// Exists mainly to help with the [`ChaosSelector`] matching, and also distinguishes between
    /// read and write directions.
    #[tracing::instrument(level = Level::DEBUG, skip(self), ret)]
    fn chaos_latency_for_connection(
        &self,
        interceptor_id: InterceptorId,
        direction: ChaosLatencyDirection,
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
                } => match direction {
                    ChaosLatencyDirection::Read => effect.read_latency_duration(),
                    ChaosLatencyDirection::Write => effect.write_latency_duration(),
                },
                _ => None,
            },
        )
    }

    /// Connection error to apply to an already intercepted connection.
    ///
    /// [`ConnectionErrorType::Refused`] is only meaningful while establishing a connection. Once
    /// the connection is established, only failures that can happen during I/O are applied.
    #[tracing::instrument(level = Level::DEBUG, skip(self), ret)]
    pub(super) fn chaos_connection_error_for_ongoing_connection(
        &self,
        interceptor_id: InterceptorId,
    ) -> Option<ChaosEffectConnectionError> {
        let connection_info = self.connection_info(interceptor_id)?;

        self.chaos_effect_for_address(
            &connection_info.remote_address,
            interceptor_id.protocol,
            connection_info.hostname.as_ref(),
            |selector| match selector {
                ChaosSelector::Tcp {
                    effect: TcpChaosEffect::ConnectionError(effect),
                    ..
                } if matches!(
                    effect.error_type,
                    ConnectionErrorType::Reset | ConnectionErrorType::TimedOut
                ) =>
                {
                    Some(effect.clone())
                }
                _ => None,
            },
        )
    }
}
