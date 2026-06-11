use std::{sync::atomic::Ordering, time::Duration};

use mirrord_intproxy_protocol::*;

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
        delay: Duration,
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

        match rule.selector {
            ChaosSelector::Tcp {
                percentage,
                effect:
                    TcpChaosEffect::ConnectionError(effect @ ChaosEffectConnError { error_type, after }),
                ..
            } => {
                let hit_count = rule.hit_count.fetch_add(1, Ordering::Relaxed);

                percentage.should_apply_for_hit(hit_count).then(|| {
                    OutgoingThingToDo::ConnectionError {
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
                    }
                })
            }
            ChaosSelector::Tcp {
                percentage,
                effect: TcpChaosEffect::Latency(effect @ ChaosEffectLatency { delay, jitter }),
                ..
            } => {
                let hit_count = rule.hit_count.fetch_add(1, Ordering::Relaxed);

                percentage
                    .should_apply_for_hit(hit_count)
                    .then(|| OutgoingThingToDo::Latency {
                        delay: {
                            let applied_n_times = percentage.applied_before_hit(hit_count);

                            delay + jitter.saturating_mul(applied_n_times)
                        },
                        effect: effect.clone(),
                    })
            }
            ChaosSelector::Tcp {
                percentage,
                effect: TcpChaosEffect::Degradation,
                ..
            } => {
                let hit_count = rule.hit_count.fetch_add(1, Ordering::Relaxed);
                percentage.should_apply_for_hit(hit_count).then(|| ());
                todo!()
            }
            _ => todo!(),
        }
    }
}
