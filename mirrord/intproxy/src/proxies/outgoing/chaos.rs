use std::{
    ops::{ControlFlow, Not},
    sync::atomic::Ordering,
    time::Duration,
};

use mirrord_intproxy_protocol::{
    LayerId, MessageId, OutgoingConnectRequest, OutgoingResponse, ProxyToLayerMessage,
};
use mirrord_protocol::ResponseError;
use tokio::time::sleep;
use tracing::Level;

use crate::{
    background_tasks::MessageBus,
    main_tasks::ToLayer,
    proxies::outgoing::{InterceptorId, OutgoingProxy},
    session_monitor::chaos::rules::{ChaosEffectConnectionError, ChaosSelector, TcpChaosEffect},
};

#[derive(Debug)]
pub enum OutgoingChaosEffect {
    Latency { wait_for: Duration },
    ConnectionError { to_layer: ToLayer, after: Duration },
}

impl OutgoingProxy {
    #[tracing::instrument(level = Level::INFO, skip(message_bus), ret)]
    pub(super) async fn apply_chaos_effect(
        which_effect: OutgoingChaosEffect,
        message_bus: &mut MessageBus<OutgoingProxy>,
    ) -> ControlFlow<()> {
        match which_effect {
            OutgoingChaosEffect::Latency { wait_for } => {
                sleep(wait_for).await;

                ControlFlow::Continue(())
            }
            OutgoingChaosEffect::ConnectionError { to_layer, after } => {
                sleep(after).await;
                message_bus.send(to_layer).await;

                ControlFlow::Break(())
            }
        }
    }

    #[tracing::instrument(level = Level::INFO, skip(self), ret)]
    pub(super) fn chaos_effect_for_connect(
        &self,
        request: &OutgoingConnectRequest,
        message_id: MessageId,
        layer_id: LayerId,
    ) -> Option<OutgoingChaosEffect> {
        self.chaos_rx.inspect_rules(|rules| {
            let rule = rules
                .iter()
                .filter(|rule| {
                    rule.applies_to_address(
                        &request.remote_address,
                        request.protocol,
                        request.hostname.as_ref(),
                    )
                })
                .max_by_key(|rule| rule.priority)?;

            if rule.get_selector_percentage().roll_for_hit().not() {
                return None;
            }

            let effect = match rule.selector.clone() {
                ChaosSelector::Tcp {
                    effect:
                        TcpChaosEffect::ConnectionError(ChaosEffectConnectionError {
                            error_type,
                            after,
                        }),
                    ..
                } => Some(OutgoingChaosEffect::ConnectionError {
                    to_layer: ToLayer {
                        message_id,
                        layer_id,
                        message: ProxyToLayerMessage::Outgoing(OutgoingResponse::Connect(Err(
                            ResponseError::RemoteIO(error_type.into()),
                        ))),
                    },
                    after,
                }),
                ChaosSelector::Tcp {
                    effect: TcpChaosEffect::Latency(effect),
                    ..
                } => Some(OutgoingChaosEffect::Latency {
                    wait_for: effect.latency_duration(),
                }),
                ChaosSelector::Tcp {
                    effect: TcpChaosEffect::Degradation,
                    ..
                } => {
                    tracing::warn!("Degradation not implemented!");
                    None
                }
                _ => None,
            };

            if effect.is_some() {
                let _ = rule.hit_count.fetch_add(1, Ordering::Relaxed);
            }

            effect
        })
    }

    #[tracing::instrument(level = Level::INFO, skip(self), ret)]
    pub(super) fn chaos_effect_for_write(
        &self,
        interceptor_id: InterceptorId,
    ) -> Option<OutgoingChaosEffect> {
        let connection_info = self.foo_chaos(interceptor_id)?;

        self.chaos_rx.inspect_rules(|rules| {
            let rule = rules
                .iter()
                .filter(|rule| {
                    rule.applies_to_address(
                        &connection_info.remote_address,
                        interceptor_id.protocol,
                        connection_info.hostname.as_ref(),
                    )
                })
                .max_by_key(|rule| rule.priority)?;

            if !rule.get_selector_percentage().roll_for_hit() {
                return None;
            }

            let _ = rule.hit_count.fetch_add(1, Ordering::Relaxed);

            let effect = match rule.selector {
                ChaosSelector::Tcp {
                    effect: TcpChaosEffect::Latency(effect),
                    ..
                } => Some(OutgoingChaosEffect::Latency {
                    wait_for: effect.latency_duration(),
                }),
                _ => None,
            };

            if effect.is_some() {
                let _ = rule.hit_count.fetch_add(1, Ordering::Relaxed);
            }

            effect
        })
    }
}
