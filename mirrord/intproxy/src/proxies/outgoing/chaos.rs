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

pub enum OutgoingThingToDo {
    Latency {
        effect: ChaosEffectLatency,
    },
    ConnectionError {
        to_layer: ToLayer,
        effect: ChaosEffectConnError,
    },
}

impl ApplyChaosRuleLol for OutgoingProxyMessage {
    type WhatToDo = OutgoingThingToDo;

    fn chaos_effect(&self, rules: &ChaosRuleList) -> Option<Self::WhatToDo> {
        let Self::Layer(
            OutgoingRequest::Connect(OutgoingConnectRequest {
                remote_address,
                protocol,
            }),
            message_id,
            layer_id,
        ) = self
        else {
            return None;
        };

        let Some(rule) = rules
            .iter()
            .filter(|rule| rule.applies_to_address(remote_address, *protocol))
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
                rule.hit_count.fetch_add(1, Ordering::Relaxed);

                Some(OutgoingThingToDo::ConnectionError {
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
                })
            }
            ChaosSelector::Tcp {
                percentage,
                effect: TcpChaosEffect::Latency(effect),
                ..
            } => {
                rule.hit_count.fetch_add(1, Ordering::Relaxed);

                Some(OutgoingThingToDo::Latency {
                    effect: effect.clone(),
                })
            }
            ChaosSelector::Tcp {
                percentage,
                effect: TcpChaosEffect::Degradation,
                ..
            } => {
                todo!()
            }
            _ => todo!(),
        }
    }
}
