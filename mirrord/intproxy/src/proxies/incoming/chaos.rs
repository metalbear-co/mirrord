use std::time::Duration;

use crate::{
    main_tasks::ToLayer,
    proxies::incoming::IncomingProxyMessage,
    session_monitor::chaos::{ApplyChaosRuleLol, ChaosRuleList},
};

pub enum IncomingLol {
    Delay(Duration),
    ToLayer(ToLayer),
}

impl ApplyChaosRuleLol for IncomingProxyMessage {
    type WhatToDo = IncomingLol;

    fn chaos_effect(&self, rules: &ChaosRuleList) -> Option<Self::WhatToDo> {
        match self {
            IncomingProxyMessage::LayerRequest(_, layer_id, incoming_request) => todo!(),
            IncomingProxyMessage::LayerForked(layer_forked) => todo!(),
            IncomingProxyMessage::LayerClosed(layer_closed) => todo!(),
            IncomingProxyMessage::AgentMirror(daemon_tcp) => todo!(),
            IncomingProxyMessage::AgentSteal(daemon_tcp) => todo!(),
            IncomingProxyMessage::AgentProtocolVersion(version) => todo!(),
            IncomingProxyMessage::ConnectionRefresh(connection_refresh) => todo!(),
        }
    }
}
