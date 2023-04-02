use std::{
    collections::HashMap,
    pin::Pin,
    task::{Context, Poll},
};

use async_trait::async_trait;
use futures::Stream;
use mirrord_protocol::{
    api::{agent_server, BincodeMessage, Empty},
    codec::{ClientMessage, DaemonMessage, GetEnvVarsRequest},
};
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio_stream::wrappers::BroadcastStream;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, trace, warn};

use crate::{
    dns::DnsRequest,
    env::select_env_vars,
    error::AgentError,
    file::FileManager,
    outgoing::{udp::UdpOutgoingApi, TcpOutgoingApi},
    sniffer::{SnifferCommand, TcpSnifferApi},
    steal::{api::TcpStealerApi, StealerCommand},
    util::ClientId,
};

type Result<T, E = AgentError> = std::result::Result<T, E>;

const CHANNEL_SIZE: usize = 1024;

pub struct ClientConnection {
    id: ClientId,
    file_manager: Mutex<FileManager>,
    stream_responce: broadcast::Sender<DaemonMessage>,
    tcp_sniffer_api: Option<Mutex<TcpSnifferApi>>,
    tcp_stealer_api: Mutex<TcpStealerApi>,
    tcp_outgoing_api: Mutex<TcpOutgoingApi>,
    udp_outgoing_api: Mutex<UdpOutgoingApi>,
    dns_sender: mpsc::Sender<DnsRequest>,
    env: HashMap<String, String>,
    cancellation_token: CancellationToken,
}

impl ClientConnection {
    pub async fn create(
        id: ClientId,
        pid: Option<u64>,
        ephemeral: bool,
        sniffer_command_sender: mpsc::Sender<SnifferCommand>,
        stealer_command_sender: mpsc::Sender<StealerCommand>,
        dns_sender: mpsc::Sender<DnsRequest>,
        env: HashMap<String, String>,
    ) -> Result<Self> {
        let cancellation_token = CancellationToken::new();

        let file_manager = Mutex::new(match pid {
            Some(_) => FileManager::new(pid),
            None if ephemeral => FileManager::new(Some(1)),
            None => FileManager::new(None),
        });

        let (tcp_sender, tcp_receiver) = mpsc::channel(CHANNEL_SIZE);

        let tcp_sniffer_api = TcpSnifferApi::new(
            id,
            sniffer_command_sender,
            tcp_receiver,
            tcp_sender,
        )
        .await
        .map(Mutex::new)
        .inspect_err(|err| {
            warn!("Failed to create TcpSnifferApi: {err}, this could be due to kernel version.")
        })
        .ok();

        let tcp_outgoing_api = TcpOutgoingApi::new(pid).into();
        let udp_outgoing_api = UdpOutgoingApi::new(pid).into();

        let tcp_stealer_api =
            TcpStealerApi::new(id, stealer_command_sender, mpsc::channel(CHANNEL_SIZE))
                .await?
                .into();

        let (stream_responce, _) = broadcast::channel(CHANNEL_SIZE);

        Ok(ClientConnection {
            cancellation_token,
            id,
            file_manager,
            stream_responce,
            tcp_sniffer_api,
            tcp_stealer_api,
            tcp_outgoing_api,
            udp_outgoing_api,
            dns_sender,
            env,
        })
    }

    async fn respond(&self, message: DaemonMessage) -> Result<()> {
        self.stream_responce.send(message)?;

        Ok(())
    }
}

#[async_trait]
impl agent_server::Agent for ClientConnection {
    type DaemonMessageStream = DaemonMessageStream;

    async fn client_message(
        &self,
        request: tonic::Request<BincodeMessage>,
    ) -> Result<tonic::Response<Empty>, tonic::Status> {
        let message = request.into_inner().as_bincode().map_err(|err| {
            tonic::Status::invalid_argument(format!("Unable to decode message {err}"))
        })?;

        match message {
            ClientMessage::FileRequest(req) => {
                if let Some(response) = self.file_manager.lock().await.handle_message(req)? {
                    self.respond(DaemonMessage::File(response))
                        .await
                        .inspect_err(|fail| {
                            error!(
                                "handle_client_message -> Failed responding to file message {:#?}!",
                                fail
                            )
                        })
                        .map_err(|err| tonic::Status::from_error(Box::new(err)))?
                }
            }
            ClientMessage::TcpOutgoing(layer_message) => {
                self.tcp_outgoing_api
                    .lock()
                    .await
                    .layer_message(layer_message)
                    .await?
            }
            ClientMessage::UdpOutgoing(layer_message) => {
                self.udp_outgoing_api
                    .lock()
                    .await
                    .layer_message(layer_message)
                    .await?
            }
            ClientMessage::GetEnvVarsRequest(GetEnvVarsRequest {
                env_vars_filter,
                env_vars_select,
            }) => {
                debug!(
                    "ClientMessage::GetEnvVarsRequest client id {:?} filter {:?} select {:?}",
                    self.id, env_vars_filter, env_vars_select
                );

                let env_vars_result = select_env_vars(&self.env, env_vars_filter, env_vars_select);

                self.respond(DaemonMessage::GetEnvVarsResponse(env_vars_result))
                    .await?
            }
            ClientMessage::GetAddrInfoRequest(request) => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let dns_request = DnsRequest::new(request, tx);
                self.dns_sender
                    .send(dns_request)
                    .await
                    .map_err(AgentError::from)?;

                trace!("waiting for answer from dns thread");
                let response = rx.await.map_err(AgentError::from)?;

                trace!("GetAddrInfoRequest -> response {:#?}", response);

                self.respond(DaemonMessage::GetAddrInfoResponse(response))
                    .await?
            }
            ClientMessage::Ping => self.respond(DaemonMessage::Pong).await?,
            ClientMessage::Tcp(message) => {
                if let Some(sniffer_api) = self.tcp_sniffer_api.as_ref() {
                    sniffer_api
                        .lock()
                        .await
                        .handle_client_message(message)
                        .await?
                } else {
                    warn!("received tcp sniffer request while not available");
                    return Err(AgentError::SnifferApiError.into());
                }
            }
            ClientMessage::TcpSteal(message) => {
                self.tcp_stealer_api
                    .lock()
                    .await
                    .handle_client_message(message)
                    .await?;
            }
            ClientMessage::Close => {
                self.cancellation_token.cancel();
            }
        }

        Ok(tonic::Response::new(Empty::default()))
    }

    async fn daemon_message(
        &self,
        _request: tonic::Request<Empty>,
    ) -> Result<tonic::Response<Self::DaemonMessageStream>, tonic::Status> {
        let stream = BroadcastStream::new(self.stream_responce.subscribe());
        Ok(tonic::Response::new(DaemonMessageStream(stream)))
    }
}

pub struct DaemonMessageStream(BroadcastStream<DaemonMessage>);

impl Stream for DaemonMessageStream {
    type Item = Result<BincodeMessage, tonic::Status>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        BroadcastStream::poll_next(Pin::new(&mut self.0), cx).map(|opt_result| {
            opt_result.and_then(|result| result.ok()).map(|message| {
                BincodeMessage::from_bincode(message)
                    .map_err(Box::new)
                    .map_err(|err| tonic::Status::from_error(Box::new(err)))
            })
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}
