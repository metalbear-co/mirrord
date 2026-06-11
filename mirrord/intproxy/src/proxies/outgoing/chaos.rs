use std::{sync::atomic::Ordering, time::Duration};

use mirrord_intproxy_protocol::*;
use rand::random_range;

use crate::{
    main_tasks::ToLayer,
    proxies::outgoing::*,
    session_monitor::chaos::{
        ApplyChaosRuleLol, ChaosRuleList,
        rules::{ChaosEffectConnError, ChaosEffectLatency},
    },
};

#[derive(Debug)]
pub enum OutgoingThingToDo {
    Latency {
        total_delay: Duration,
        effect: ChaosEffectLatency,
    },
    ConnectionError {
        to_layer: ToLayer,
        effect: ChaosEffectConnError,
    },
}

impl ApplyChaosRuleLol for OutgoingProxyMessage {
    type WhatToDo = OutgoingThingToDo;

    #[tracing::instrument(level = Level::INFO, skip(self), ret)]
    fn chaos_effect(&self, rules: &ChaosRuleList) -> Option<Self::WhatToDo> {
        let Self::Layer(
            OutgoingRequest::Connect(OutgoingConnectRequest {
                remote_address,
                protocol,
                hostname,
            }),
            message_id,
            layer_id,
        ) = self
        else {
            return None;
        };

        let Some(rule) = rules
            .iter()
            .filter(|rule| rule.applies_to_address(remote_address, *protocol, hostname.as_ref()))
            .max_by_key(|rule| rule.priority)
        else {
            return None;
        };

        if rule.get_selector_percentage().roll_for_hit() {
            let _ = rule.hit_count.fetch_add(1, Ordering::Relaxed);
            return match rule.selector {
                ChaosSelector::Tcp {
                    effect: TcpChaosEffect::ConnectionError(effect @ ChaosEffectConnError { .. }),
                    ..
                } => Some(OutgoingThingToDo::ConnectionError {
                    to_layer: ToLayer {
                        message_id: *message_id,
                        layer_id: *layer_id,
                        message: ProxyToLayerMessage::Outgoing(OutgoingResponse::Connect(Err(
                            ResponseError::Remote(RemoteError::ConnectTimedOut(
                                remote_address.clone(),
                            )),
                        ))),
                    },
                    effect: effect.clone(),
                }),
                ChaosSelector::Tcp {
                    effect: TcpChaosEffect::Latency(effect @ ChaosEffectLatency { delay, jitter }),
                    ..
                } => Some(OutgoingThingToDo::Latency {
                    total_delay: delay + jitter.div_f32(100.).saturating_mul(random_range(0..=100)),
                    effect: effect.clone(),
                }),
                ChaosSelector::Tcp {
                    effect: TcpChaosEffect::Degradation,
                    ..
                } => {
                    todo!()
                }
                _ => todo!(),
            };
        }
        None
    }
}
