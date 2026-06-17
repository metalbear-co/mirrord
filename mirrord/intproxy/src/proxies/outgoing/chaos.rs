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

    fn chaos_effect_for_address(
        &self,
        remote_address: &SocketAddress,
        protocol: NetProtocol,
        hostname: Option<&String>,
        to_effect: impl FnOnce(&ChaosSelector) -> Option<OutgoingChaosEffect>,
    ) -> Option<OutgoingChaosEffect> {
        self.chaos_rx.inspect_rules(|rules| {
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
        })
    }

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

    #[tracing::instrument(level = Level::INFO, skip(self), ret)]
    pub(super) fn chaos_effect_for_connect(
        &self,
        request: &OutgoingConnectRequest,
        message_id: MessageId,
        layer_id: LayerId,
    ) -> Option<OutgoingChaosEffect> {
        self.chaos_effect_for_address(
            &request.remote_address,
            request.protocol,
            request.hostname.as_ref(),
            |selector| match selector {
                ChaosSelector::Tcp {
                    effect: TcpChaosEffect::ConnectionError(effect),
                    ..
                } => Some(Self::connection_error_effect(effect, message_id, layer_id)),

                ChaosSelector::Tcp {
                    effect: TcpChaosEffect::Latency(effect),
                    ..
                } => Some(OutgoingChaosEffect::Latency {
                    wait_for: effect.latency_duration(),
                }),
                _ => None,
            },
        )
    }

    #[tracing::instrument(level = Level::INFO, skip(self), ret)]
    pub(super) fn chaos_effect_for_write(
        &self,
        interceptor_id: InterceptorId,
    ) -> Option<OutgoingChaosEffect> {
        let connection_info = self.connection_info(interceptor_id)?;

        self.chaos_effect_for_address(
            &connection_info.remote_address,
            interceptor_id.protocol,
            connection_info.hostname.as_ref(),
            |selector| match selector {
                ChaosSelector::Tcp {
                    effect: TcpChaosEffect::Latency(effect),
                    ..
                } => Some(OutgoingChaosEffect::Latency {
                    wait_for: effect.latency_duration(),
                }),
                _ => None,
            },
        )
    }
}
