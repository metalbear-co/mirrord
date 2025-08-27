//! The most basic proxying logic. Handles cases when the only job to do in the internal proxy is to
//! pass requests and responses between the layer and the agent.

use std::collections::HashMap;

use mirrord_intproxy_protocol::{LayerId, MessageId, ProxyToLayerMessage};
use mirrord_protocol::{
    ClientMessage, DaemonMessage, DnsLookupError, GetEnvVarsRequest, RemoteResult,
    ResolveErrorKindInternal, ResponseError,
    dns::{ADDRINFO_V2_VERSION, AddressFamily, GetAddrInfoRequestV2, GetAddrInfoResponse},
};
use semver::Version;
use thiserror::Error;
use tracing::Level;

use crate::{
    ProxyMessage,
    background_tasks::{BackgroundTask, MessageBus},
    error::{UnexpectedAgentMessage, agent_lost_io_error},
    main_tasks::ToLayer,
    request_queue::RequestQueue,
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
pub enum SimpleProxyError {
    #[error(transparent)]
    UnexpectedAgentMessage(#[from] UnexpectedAgentMessage),

    #[error(
        "mirrord-agent failed DNS lookup, permission denied. \
        This indicates that your Kubernetes cluster is hardened, \
        and you need to use privileged agents (agent.privileged configuration option)"
    )]
    DnsPermissionDenied,
}

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
pub struct SimpleProxy {
    /// For [`GetAddrInfoRequestV2`]s.
    addr_info_reqs: RequestQueue,
    /// For [`GetEnvVarsRequest`]s.
    get_env_reqs: RequestQueue,
    /// [`mirrord_protocol`] version negotiated with the agent.
    /// Determines whether we can use `GetAddrInfoRequestV2`.
    protocol_version: Option<Version>,
    /// Whether to consider permission errors in DNS lookup to be fatal.
    dns_permission_error_fatal: bool,
}

impl SimpleProxy {
    pub fn new(dns_permission_error_fatal: bool) -> Self {
        Self {
            addr_info_reqs: Default::default(),
            get_env_reqs: Default::default(),
            protocol_version: Default::default(),
            dns_permission_error_fatal,
        }
    }

    fn set_protocol_version(&mut self, version: Version) {
        self.protocol_version.replace(version);
    }

    /// Returns whether [`mirrord_protocol`] version allows for a V2 addrinfo request.
    fn addr_info_v2(&self) -> bool {
        self.protocol_version
            .as_ref()
            .is_some_and(|version| ADDRINFO_V2_VERSION.matches(version))
    }

    #[tracing::instrument(level = Level::INFO, skip_all, ret, err)]
    async fn handle_connection_refresh(
        &mut self,
        message_bus: &mut MessageBus<Self>,
    ) -> Result<(), SimpleProxyError> {
        tracing::debug!(
            num_responses = self.addr_info_reqs.len(),
            "Flushing error responses to GetAddrInfoRequests"
        );
        while let Some((message_id, layer_id)) = self.addr_info_reqs.pop_front() {
            message_bus
                .send(ToLayer::from(AgentLostSimpleResponse::addr_info(
                    layer_id, message_id,
                )))
                .await;
        }

        tracing::debug!(
            num_responses = self.get_env_reqs.len(),
            "Flushing error responses to GetEnvVarsRequests"
        );
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

    #[tracing::instrument(level = Level::INFO, name = "simple_proxy_main_loop", skip_all, ret, err)]
    async fn run(&mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        while let Some(msg) = message_bus.recv().await {
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
                                "The agent version you're using does not support DNS \
                                queries for IPv6 addresses. This version will only fetch IPv4 \
                                address. Please update to a newer agent image for better IPv6 \
                                support."
                            )
                        }
                        message_bus
                            .send(ClientMessage::GetAddrInfoRequest(req.into()))
                            .await;
                    }
                }
                SimpleProxyMessage::AddrInfoRes(GetAddrInfoResponse(Err(
                    ResponseError::DnsLookup(DnsLookupError {
                        kind: ResolveErrorKindInternal::PermissionDenied,
                    }),
                ))) if self.dns_permission_error_fatal => {
                    return Err(SimpleProxyError::DnsPermissionDenied);
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

        tracing::debug!("Message bus closed, exiting");

        Ok(())
    }
}
