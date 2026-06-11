use std::{ops::ControlFlow, sync::atomic::Ordering, time::Duration};

use mirrord_intproxy_protocol::{
    LayerId, MessageId, OutgoingConnectRequest, OutgoingResponse, ProxyToLayerMessage,
};
use mirrord_protocol::{RemoteError, ResponseError};
use rand::random_range;
use tokio::time::sleep;
use tracing::Level;

use crate::{
    background_tasks::MessageBus,
    main_tasks::ToLayer,
    proxies::outgoing::{InterceptorId, OutgoingProxy},
    session_monitor::chaos::rules::{
        ChaosEffectConnError, ChaosEffectLatency, ChaosSelector, TcpChaosEffect,
    },
};

#[derive(Debug)]
pub enum OutgoingChaos {
    Latency {
        total_delay: Duration,
        effect: ChaosEffectLatency,
    },
    ConnectionError {
        to_layer: ToLayer,
        effect: ChaosEffectConnError,
    },
}

impl OutgoingProxy {
    #[tracing::instrument(level = Level::INFO, skip(message_bus), ret)]
    pub(super) async fn apply_chaos_effect(
        chaos_reigns: OutgoingChaos,
        message_bus: &mut MessageBus<OutgoingProxy>,
    ) -> ControlFlow<()> {
        match chaos_reigns {
            OutgoingChaos::Latency { total_delay, .. } => {
                sleep(total_delay).await;

                ControlFlow::Continue(())
            }
            OutgoingChaos::ConnectionError {
                to_layer,
                effect: ChaosEffectConnError { after, .. },
            } => {
                tracing::info!(?after, ?to_layer, "We have a match for a connection error");
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
    ) -> Option<OutgoingChaos> {
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

            if !rule.get_selector_percentage().roll_for_hit() {
                return None;
            }

            let _ = rule.hit_count.fetch_add(1, Ordering::Relaxed);

            match rule.selector {
                ChaosSelector::Tcp {
                    effect: TcpChaosEffect::ConnectionError(effect),
                    ..
                } => Some(OutgoingChaos::ConnectionError {
                    to_layer: ToLayer {
                        message_id,
                        layer_id,
                        message: ProxyToLayerMessage::Outgoing(OutgoingResponse::Connect(Err(
                            ResponseError::Remote(RemoteError::ConnectTimedOut(
                                request.remote_address.clone(),
                            )),
                        ))),
                    },
                    effect,
                }),
                ChaosSelector::Tcp {
                    effect: TcpChaosEffect::Latency(effect),
                    ..
                } => Some(OutgoingChaos::Latency {
                    total_delay: latency_duration(effect),
                    effect,
                }),
                ChaosSelector::Tcp {
                    effect: TcpChaosEffect::Degradation,
                    ..
                } => todo!(),
                _ => None,
            }
        })
    }

    #[tracing::instrument(level = Level::INFO, skip(self), ret)]
    pub(super) fn chaos_effect_for_write(
        &self,
        interceptor_id: InterceptorId,
    ) -> Option<OutgoingChaos> {
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

            match rule.selector {
                ChaosSelector::Tcp {
                    effect: TcpChaosEffect::Latency(effect),
                    ..
                } => Some(OutgoingChaos::Latency {
                    total_delay: latency_duration(effect),
                    effect,
                }),
                _ => None,
            }
        })
    }
}

fn latency_duration(effect: ChaosEffectLatency) -> Duration {
    effect.delay
        + effect
            .jitter
            .div_f32(100.)
            .saturating_mul(random_range(0..=100))
}
