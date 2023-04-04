use std::collections::HashMap;

use async_trait::async_trait;
use futures::Stream;
use hyper_14::server::conn::Http;
use mirrord_protocol::{
    api::{agent_server, agent_server::AgentServer, BincodeMessage},
    codec::{ClientMessage, DaemonMessage},
    GetEnvVarsRequest,
};
use tokio::{net::TcpStream, sync::mpsc};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
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

pub mod streaming;

type Result<T, E = AgentError> = std::result::Result<T, E>;

const CHANNEL_SIZE: usize = 1024;

pub struct ClientConnection<S> {
    id: ClientId,
    client_messages: streaming::ClientMessageStream<S>,
    file_manager: FileManager,
    stream_responce: mpsc::Sender<DaemonMessage>,
    tcp_sniffer_api: Option<TcpSnifferApi>,
    tcp_stealer_api: TcpStealerApi,
    tcp_outgoing_api: TcpOutgoingApi,
    udp_outgoing_api: UdpOutgoingApi,
    dns_sender: mpsc::Sender<DnsRequest>,
    env: HashMap<String, String>,
    cancellation_token: CancellationToken,
}

impl<S> ClientConnection<S>
where
    S: Stream<Item = Result<BincodeMessage, tonic::Status>> + Unpin,
{
    pub async fn create(
        id: ClientId,
        client_messages: streaming::ClientMessageStream<S>,
        stream_responce: mpsc::Sender<DaemonMessage>,
        pid: Option<u64>,
        ephemeral: bool,
        sniffer_command_sender: mpsc::Sender<SnifferCommand>,
        stealer_command_sender: mpsc::Sender<StealerCommand>,
        dns_sender: mpsc::Sender<DnsRequest>,
        env: HashMap<String, String>,
        cancellation_token: CancellationToken,
    ) -> Result<Self> {
        let file_manager = match pid {
            Some(_) => FileManager::new(pid),
            None if ephemeral => FileManager::new(Some(1)),
            None => FileManager::new(None),
        };

        let (tcp_sender, tcp_receiver) = mpsc::channel(CHANNEL_SIZE);

        let tcp_sniffer_api = TcpSnifferApi::new(
            id,
            sniffer_command_sender,
            tcp_receiver,
            tcp_sender,
        )
        .await
        .inspect_err(|err| {
            warn!("Failed to create TcpSnifferApi: {err}, this could be due to kernel version.")
        })
        .ok();

        let tcp_outgoing_api = TcpOutgoingApi::new(pid);
        let udp_outgoing_api = UdpOutgoingApi::new(pid);

        let tcp_stealer_api =
            TcpStealerApi::new(id, stealer_command_sender, mpsc::channel(CHANNEL_SIZE)).await?;

        Ok(ClientConnection {
            cancellation_token,
            id,
            file_manager,
            client_messages,
            stream_responce,
            tcp_sniffer_api,
            tcp_stealer_api,
            tcp_outgoing_api,
            udp_outgoing_api,
            dns_sender,
            env,
        })
    }

    pub async fn start(&mut self) -> Result<(), tonic::Status> {
        loop {
            tokio::select! {
                message = self.client_messages.next() => {
                    match message {
                        Some(message) => self.handle_client_message(message?).await?,
                        None => {
                            break
                        }
                    }
                }
                // poll the sniffer API only when it's available
                // exit when it stops (means something bad happened if
                // it ran and then stopped)
                message = async {
                    if let Some(ref mut sniffer_api) = self.tcp_sniffer_api {
                        sniffer_api.recv().await
                    } else {
                        unreachable!()
                    }
                }, if self.tcp_sniffer_api.is_some()=> {
                    if let Some(message) = message {
                        self.respond(DaemonMessage::Tcp(message)).await?;
                    } else {
                        error!("tcp sniffer stopped?");
                        break;
                    }
                },
                message = self.tcp_stealer_api.recv() => {
                    if let Some(message) = message {
                        self.respond(DaemonMessage::TcpSteal(message)).await?;
                    } else {
                        error!("tcp stealer stopped?");
                        break;
                    }
                },
                message = self.tcp_outgoing_api.daemon_message() => {
                    self.respond(DaemonMessage::TcpOutgoing(message?)).await?;
                },
                message = self.udp_outgoing_api.daemon_message() => {
                    self.respond(DaemonMessage::UdpOutgoing(message?)).await?;
                },
                _ = self.cancellation_token.cancelled() => {
                    break;
                }
            }
        }

        Ok(())
    }

    pub async fn handle_client_message(&mut self, message: ClientMessage) -> Result<()> {
        match message {
            ClientMessage::FileRequest(req) => {
                if let Some(response) = self.file_manager.handle_message(req)? {
                    self.respond(DaemonMessage::File(response))
                        .await
                        .inspect_err(|fail| {
                            error!(
                                "handle_client_message -> Failed responding to file message {fail:#?}!"
                            )
                        })?
                }
            }
            ClientMessage::TcpOutgoing(layer_message) => {
                self.tcp_outgoing_api.layer_message(layer_message).await?
            }
            ClientMessage::UdpOutgoing(layer_message) => {
                self.udp_outgoing_api.layer_message(layer_message).await?
            }
            ClientMessage::GetEnvVarsRequest(GetEnvVarsRequest {
                env_vars_filter,
                env_vars_select,
            }) => {
                debug!(
                    "ClientMessage::GetEnvVarsRequest client id {:?} filter {env_vars_filter:?}select {env_vars_select:?}",
                    self.id
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

                trace!("GetAddrInfoRequest -> response {response:#?}");

                self.respond(DaemonMessage::GetAddrInfoResponse(response))
                    .await?
            }
            ClientMessage::Ping => self.respond(DaemonMessage::Pong).await?,
            ClientMessage::Tcp(message) => {
                if let Some(ref mut sniffer_api) = self.tcp_sniffer_api {
                    sniffer_api.handle_client_message(message).await?
                } else {
                    warn!("received tcp sniffer request while not available");
                    return Err(AgentError::SnifferApiError.into());
                }
            }
            ClientMessage::TcpSteal(message) => {
                self.tcp_stealer_api.handle_client_message(message).await?;
            }
            ClientMessage::Close => {
                self.cancellation_token.cancel();
            }
        }

        Ok(())
    }

    async fn respond(&self, message: DaemonMessage) -> Result<()> {
        self.stream_responce.send(message).await?;

        Ok(())
    }
}

pub struct ClientConnectionHandler {
    id: ClientId,
    pid: Option<u64>,
    ephemeral: bool,
    sniffer_command_sender: mpsc::Sender<SnifferCommand>,
    stealer_command_sender: mpsc::Sender<StealerCommand>,
    dns_sender: mpsc::Sender<DnsRequest>,
    env: HashMap<String, String>,
    cancellation_token: CancellationToken,
}

impl ClientConnectionHandler {
    pub fn new(
        id: ClientId,
        pid: Option<u64>,
        ephemeral: bool,
        sniffer_command_sender: mpsc::Sender<SnifferCommand>,
        stealer_command_sender: mpsc::Sender<StealerCommand>,
        dns_sender: mpsc::Sender<DnsRequest>,
        env: HashMap<String, String>,
        cancellation_token: CancellationToken,
    ) -> Self {
        ClientConnectionHandler {
            id,
            pid,
            ephemeral,
            sniffer_command_sender,
            stealer_command_sender,
            dns_sender,
            env,
            cancellation_token,
        }
    }

    pub async fn serve(self, tcp_stream: TcpStream) -> Result<()> {
        let service = AgentServer::new(self);

        if let Err(http_err) = Http::new()
            .http1_only(true)
            .http1_keep_alive(true)
            .serve_connection(tcp_stream, service)
            .await
        {
            error!("Error while serving HTTP connection: {}", http_err);
        }

        Ok(())
    }
}

#[async_trait]
impl agent_server::Agent for ClientConnectionHandler {
    type LayerConnectStream = streaming::DaemonMessageStream;

    async fn layer_connect(
        &self,
        request: tonic::Request<tonic::Streaming<BincodeMessage>>,
    ) -> Result<tonic::Response<Self::LayerConnectStream>, tonic::Status> {
        let (response_sender, response_receiver) = mpsc::channel(CHANNEL_SIZE);

        let in_stream = streaming::ClientMessageStream(request.into_inner());
        let out_stream = streaming::DaemonMessageStream(ReceiverStream::new(response_receiver));

        let connection = ClientConnection::create(
            self.id,
            in_stream,
            response_sender,
            self.pid,
            self.ephemeral,
            self.sniffer_command_sender.clone(),
            self.stealer_command_sender.clone(),
            self.dns_sender.clone(),
            self.env.clone(),
            self.cancellation_token.child_token(),
        )
        .await?;

        Ok(tonic::Response::new(out_stream))
    }
}
