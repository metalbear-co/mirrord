use std::time::Duration;

use mirrord_intproxy_protocol::*;

use crate::{
    main_tasks::ToLayer,
    proxies::outgoing::*,
    session_monitor::chaos::{
        ApplyChaosRuleLol, ChaosRuleList, EffectOrMessageChat, rules::ChaosEffectLatency,
    },
};

pub enum OutgoingLol {
    Delay(Duration),
    ToLayer(ToLayer),
}

impl ApplyChaosRuleLol for OutgoingProxyMessage {
    type EffectLol = EffectOrMessageChat<TcpChaosEffect, ToLayer>;

    fn chaos_effect(&self, rules: &ChaosRuleList) -> Option<Self::EffectLol> {
        match self {
            OutgoingProxyMessage::Layer(
                OutgoingRequest::Connect(OutgoingConnectRequest {
                    remote_address,
                    protocol,
                }),
                message_id,
                layer_id,
            ) => rules
                .iter()
                .filter_map(|rule| match &rule.selector {
                    ChaosSelector::Tcp {
                        upstream,
                        percentage,
                        effect: TcpChaosEffect::ConnectionError(..),
                    } => upstream.matches_socket_address(remote_address).then(|| {
                        EffectOrMessageChat::BackToYouLayer(ToLayer {
                            message_id: *message_id,
                            layer_id: *layer_id,
                            message: ProxyToLayerMessage::Outgoing(OutgoingResponse::Connect(Err(
                                ResponseError::Remote(RemoteError::ConnectTimedOut(
                                    remote_address.clone(),
                                )),
                            ))),
                        })
                    }),
                    ChaosSelector::Tcp {
                        upstream,
                        percentage,
                        effect: TcpChaosEffect::Latency(latency),
                    } => upstream.matches_socket_address(remote_address).then(|| {
                        EffectOrMessageChat::Effect(TcpChaosEffect::Latency(latency.clone()))
                    }),
                    ChaosSelector::Tcp {
                        upstream,
                        percentage,
                        effect: TcpChaosEffect::Degradation,
                    } => upstream.matches_socket_address(remote_address).then(|| {
                        EffectOrMessageChat::<TcpChaosEffect, ToLayer>::BackToYouLayer(ToLayer {
                            message_id: *message_id,
                            layer_id: *layer_id,
                            message: ProxyToLayerMessage::Outgoing(OutgoingResponse::Connect(Err(
                                ResponseError::Remote(RemoteError::ConnectTimedOut(
                                    remote_address.clone(),
                                )),
                            ))),
                        })
                    }),
                    ChaosSelector::Tcp {
                        effect: TcpChaosEffect::Nothing,
                        ..
                    } => None,

                    ChaosSelector::Http {
                        upstream,
                        percentage,
                        filter,
                        effect,
                    } => todo!(),
                    ChaosSelector::Fs {
                        file_path,
                        percentage,
                        effect,
                    } => todo!(),
                    ChaosSelector::None => todo!(),
                })
                .last(),

            _ => None,
        }
    }
}
