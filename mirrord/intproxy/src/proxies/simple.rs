//! The most basic proxying logic. Handles cases when the only job to do in the internal proxy is to
//! pass requests and responses between the layer and the agent.

use std::collections::HashMap;

use mirrord_intproxy_protocol::{LayerId, MessageId, ProxyToLayerMessage};
use mirrord_protocol::{
    dns::{AddressFamily, GetAddrInfoRequestV2, GetAddrInfoResponse, ADDRINFO_V2_VERSION},
    ClientMessage, DaemonMessage, GetEnvVarsRequest, RemoteResult,
};
use semver::Version;
use thiserror::Error;

use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    error::{agent_lost_io_error, UnexpectedAgentMessage},
    main_tasks::ToLayer,
    request_queue::RequestQueue,
    ProxyMessage,
};

#[derive(Debug)]
pub enum SimpleProxyMessage {
    AddrInfoReq(MessageId, LayerId, GetAddrInfoRequestV2),
    AddrInfoRes(GetAddrInfoResponse),
    GetEnvReq(MessageId, LayerId, GetEnvVarsRequest),
    GetEnvRes(RemoteResult<HashMap<String, String>>),
    /// Protocol version was negotiated with the agent.
    ProtocolVersion(Version),
    ConnectionRefresh,
}

#[derive(Error, Debug)]
#[error(transparent)]
pub struct SimpleProxyError(#[from] UnexpectedAgentMessage);

pub enum AgentLostSimpleResponseKind {
    AddrInfo,
    GetEnv,
}

/// Lightweight (no allocations) [`ProxyMessage`] to be returned when connection with the
/// mirrord-agent is lost. Must be converted into real [`ProxyMessage`] via [`From`].
pub struct AgentLostSimpleResponse(AgentLostSimpleResponseKind, LayerId, MessageId);

impl AgentLostSimpleResponse {
    pub fn addr_info(layer_id: LayerId, message_id: MessageId) -> Self {
        AgentLostSimpleResponse(AgentLostSimpleResponseKind::AddrInfo, layer_id, message_id)
    }

    pub fn get_env(layer_id: LayerId, message_id: MessageId) -> Self {
        AgentLostSimpleResponse(AgentLostSimpleResponseKind::GetEnv, layer_id, message_id)
    }
}

impl From<AgentLostSimpleResponse> for ToLayer {
    fn from(value: AgentLostSimpleResponse) -> Self {
        let AgentLostSimpleResponse(kind, layer_id, message_id) = value;
        let error = agent_lost_io_error();

        let message = match kind {
            AgentLostSimpleResponseKind::AddrInfo => {
                ProxyToLayerMessage::GetAddrInfo(GetAddrInfoResponse(Err(error)))
            }
            AgentLostSimpleResponseKind::GetEnv => ProxyToLayerMessage::GetEnv(Err(error)),
        };

        ToLayer {
            layer_id,
            message_id,
            message,
        }
    }
}

/// For passing messages between the layer and the agent without custom internal logic.
/// Run as a [`BackgroundTask`].
#[derive(Default)]
pub struct SimpleProxy {
    /// For [`GetAddrInfoRequestV2`]s.
    addr_info_reqs: RequestQueue,
    /// For [`GetEnvVarsRequest`]s.
    get_env_reqs: RequestQueue,
    /// [`mirrord_protocol`] version negotiated with the agent.
    /// Determines whether we can use `GetAddrInfoRequestV2`.
    protocol_version: Option<Version>,
}

impl SimpleProxy {
    #[tracing::instrument(skip(self), level = tracing::Level::TRACE)]
    fn set_protocol_version(&mut self, version: Version) {
        self.protocol_version.replace(version);
    }

    /// Returns whether [`mirrord_protocol`] version allows for a V2 addrinfo request.
    fn addr_info_v2(&self) -> bool {
        self.protocol_version
            .as_ref()
            .is_some_and(|version| ADDRINFO_V2_VERSION.matches(version))
    }

    async fn handle_connection_refresh(
        &mut self,
        message_bus: &mut MessageBus<Self>,
    ) -> Result<(), SimpleProxyError> {
        while let Some((message_id, layer_id)) = self.addr_info_reqs.pop_front() {
            message_bus
                .send(ToLayer::from(AgentLostSimpleResponse::addr_info(
                    layer_id, message_id,
                )))
                .await;
        }

        while let Some((message_id, layer_id)) = self.get_env_reqs.pop_front() {
            message_bus
                .send(ToLayer::from(AgentLostSimpleResponse::get_env(
                    layer_id, message_id,
                )))
                .await;
        }

        Ok(())
    }
}

impl BackgroundTask for SimpleProxy {
    type Error = SimpleProxyError;
    type MessageIn = SimpleProxyMessage;
    type MessageOut = ProxyMessage;

    async fn run(&mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        while let Some(msg) = message_bus.recv().await {
            tracing::trace!(?msg, "new message in message_bus");

            match msg {
                SimpleProxyMessage::AddrInfoReq(message_id, session_id, req) => {
                    self.addr_info_reqs.push_back(message_id, session_id);
                    if self.addr_info_v2() {
                        message_bus
                            .send(ClientMessage::GetAddrInfoRequestV2(req))
                            .await;
                    } else {
                        if matches!(req.family, AddressFamily::Ipv6Only) {
                            tracing::warn!(
                                "The agent version you're using does not support DNS\
                                queries for IPv6 addresses. This version will only fetch IPv4\
                                address. Please update to a newer agent image for better IPv6\
                                support."
                            )
                        }
                        message_bus
                            .send(ClientMessage::GetAddrInfoRequest(req.into()))
                            .await;
                    }
                }
                SimpleProxyMessage::AddrInfoRes(res) => {
                    let (message_id, layer_id) =
                        self.addr_info_reqs.pop_front().ok_or_else(|| {
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
                    self.get_env_reqs.push_back(message_id, layer_id);
                    message_bus
                        .send(ClientMessage::GetEnvVarsRequest(req))
                        .await;
                }
                SimpleProxyMessage::GetEnvRes(res) => {
                    let (message_id, layer_id) =
                        self.get_env_reqs.pop_front().ok_or_else(|| {
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
                SimpleProxyMessage::ProtocolVersion(version) => self.set_protocol_version(version),
                SimpleProxyMessage::ConnectionRefresh => {
                    self.handle_connection_refresh(message_bus).await?
                }
            }
        }

        tracing::trace!("message bus closed, exiting");

        Ok(())
    }
}
