//! The most basic proxying logic. Handles cases when the only job to do in the internal proxy is to
//! pass requests and responses between the layer and the agent.

use std::collections::HashMap;

use mirrord_intproxy_protocol::{LayerId, MessageId, ProxyToLayerMessage};
use mirrord_protocol::{
    dns::{GetAddrInfoRequest, GetAddrInfoResponse},
    ClientMessage, DaemonMessage, GetEnvVarsRequest, RemoteResult,
};
use thiserror::Error;

use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    error::UnexpectedAgentMessage,
    main_tasks::ToLayer,
    request_queue::RequestQueue,
    ProxyMessage,
};

#[derive(Debug)]
pub enum SimpleProxyMessage {
    AddrInfoReq(MessageId, LayerId, GetAddrInfoRequest),
    AddrInfoRes(GetAddrInfoResponse),
    GetEnvReq(MessageId, LayerId, GetEnvVarsRequest),
    GetEnvRes(RemoteResult<HashMap<String, String>>),
}

#[derive(Error, Debug)]
#[error(transparent)]
pub struct SimpleProxyError(#[from] UnexpectedAgentMessage);

/// For passing messages between the layer and the agent without custom internal logic.
/// Run as a [`BackgroundTask`].
#[derive(Default)]
pub struct SimpleProxy {
    /// For [`GetAddrInfoRequest`]s.
    addr_info_reqs: RequestQueue,
    /// For [`GetEnvVarsRequest`]s.
    get_env_reqs: RequestQueue,
}

impl BackgroundTask for SimpleProxy {
    type Error = SimpleProxyError;
    type MessageIn = SimpleProxyMessage;
    type MessageOut = ProxyMessage;

    async fn run(mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        while let Some(msg) = message_bus.recv().await {
            tracing::trace!(?msg, "new message in message_bus");

            match msg {
                SimpleProxyMessage::AddrInfoReq(message_id, session_id, req) => {
                    self.addr_info_reqs.insert(message_id, session_id);
                    message_bus
                        .send(ClientMessage::GetAddrInfoRequest(req))
                        .await;
                }
                SimpleProxyMessage::AddrInfoRes(res) => {
                    let (message_id, layer_id) = self.addr_info_reqs.get().ok_or_else(|| {
                        UnexpectedAgentMessage(DaemonMessage::GetAddrInfoResponse(res.clone()))
                    })?;
                    message_bus
                        .send(ToLayer {
                            message_id,
                            message: ProxyToLayerMessage::GetAddrInfo(res),
                            layer_id,
                        })
                        .await;
                }
                SimpleProxyMessage::GetEnvReq(message_id, layer_id, req) => {
                    self.get_env_reqs.insert(message_id, layer_id);
                    message_bus
                        .send(ClientMessage::GetEnvVarsRequest(req))
                        .await;
                }
                SimpleProxyMessage::GetEnvRes(res) => {
                    let (message_id, layer_id) = self.get_env_reqs.get().ok_or_else(|| {
                        UnexpectedAgentMessage(DaemonMessage::GetEnvVarsResponse(res.clone()))
                    })?;
                    message_bus
                        .send(ToLayer {
                            message_id,
                            message: ProxyToLayerMessage::GetEnv(res),
                            layer_id,
                        })
                        .await
                }
            }
        }

        tracing::trace!("message bus closed, exiting");

        Ok(())
    }
}
