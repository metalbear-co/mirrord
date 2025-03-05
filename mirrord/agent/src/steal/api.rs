use std::{collections::HashMap, convert::Infallible};

use bytes::Bytes;
use hyper::body::Frame;
use mirrord_protocol::{
    tcp::{ChunkedResponse, DaemonTcp, HttpResponse, InternalHttpResponse, LayerTcpSteal, TcpData},
    LogMessage, RequestId,
};
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio_stream::wrappers::ReceiverStream;
use tracing::Level;

use super::{http::ReceiverStreamBody, *};
use crate::{
    error::AgentResult, metrics::HTTP_REQUEST_IN_PROGRESS_COUNT, util::ClientId,
    watched_task::TaskStatus,
};

type ResponseBodyTx = Sender<Result<Frame<Bytes>, Infallible>>;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StealerMessage {
    TcpSteal(DaemonTcp),
    LogMessage(LogMessage),
}

/// Bridges the communication between the agent and the [`TcpConnectionStealer`] task.
/// There is an API instance for each connected layer ("client"). All API instances send commands
/// On the same stealer command channel, where the layer-independent stealer listens to them.
#[derive(Debug)]
pub(crate) struct TcpStealerApi {
    /// Identifies which layer instance is associated with this API.
    client_id: ClientId,

    /// Channel that allows the agent to communicate with the stealer task.
    ///
    /// The agent controls the stealer task through this.
    command_tx: Sender<StealerCommand>,

    /// Channel that receives [`DaemonTcp`] messages from the stealer worker thread.
    ///
    /// This is where we get the messages that should be passed back to agent or layer.
    daemon_rx: Receiver<StealerMessage>,

    /// View on the stealer task's status.
    task_status: TaskStatus,

    /// [`Sender`]s that allow us to provide body [`Frame`]s of responses to filtered HTTP
    /// requests.
    ///
    /// With [`LayerTcpSteal::HttpResponseChunked`], response bodies come from the client
    /// in a series of [`ChunkedResponse::Body`] messages.
    ///
    /// Thus, we use [`ReceiverStreamBody`] for [`Response`](hyper::Response)'s body type and
    /// pipe the [`Frame`]s through an [`mpsc::channel`].
    response_body_txs: HashMap<(ConnectionId, RequestId), ResponseBodyTx>,
}

impl TcpStealerApi {
    /// Initializes a [`TcpStealerApi`] and sends a message to [`TcpConnectionStealer`] signaling
    /// that we have a new client.
    #[tracing::instrument(level = Level::TRACE, err)]
    pub(crate) async fn new(
        client_id: ClientId,
        command_tx: Sender<StealerCommand>,
        task_status: TaskStatus,
        channel_size: usize,
    ) -> AgentResult<Self> {
        let (daemon_tx, daemon_rx) = mpsc::channel(channel_size);

        command_tx
            .send(StealerCommand {
                client_id,
                command: Command::NewClient(daemon_tx),
            })
            .await?;

        Ok(Self {
            client_id,
            command_tx,
            daemon_rx,
            task_status,
            response_body_txs: HashMap::new(),
        })
    }

    /// Send `command` to stealer, with the client id of the client that is using this API instance.
    async fn send_command(&mut self, command: Command) -> AgentResult<()> {
        let command = StealerCommand {
            client_id: self.client_id,
            command,
        };

        if self.command_tx.send(command).await.is_ok() {
            Ok(())
        } else {
            Err(self.task_status.unwrap_err().await)
        }
    }

    /// Helper function that passes the [`DaemonTcp`] messages we generated in the
    /// [`TcpConnectionStealer`] task, back to the agent.
    ///
    /// Called in the `ClientConnectionHandler`.
    #[tracing::instrument(level = Level::TRACE, skip(self))]
    pub(crate) async fn recv(&mut self) -> AgentResult<StealerMessage> {
        match self.daemon_rx.recv().await {
            Some(msg) => {
                if let StealerMessage::TcpSteal(DaemonTcp::Close(close)) = &msg {
                    let all_in_progress = self.response_body_txs.len();
                    self.response_body_txs
                        .retain(|(key_id, _), _| *key_id != close.connection_id);
                    HTTP_REQUEST_IN_PROGRESS_COUNT.fetch_sub(
                        (all_in_progress - self.response_body_txs.len()) as i64,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                }
                Ok(msg)
            }
            None => Err(self.task_status.unwrap_err().await),
        }
    }

    /// Handles the conversion of [`LayerTcpSteal::PortSubscribe`], that is passed from the
    /// agent, to an internal stealer command [`Command::PortSubscribe`].
    ///
    /// The actual handling of this message is done in [`TcpConnectionStealer`].
    pub(crate) async fn port_subscribe(&mut self, port_steal: StealType) -> AgentResult<()> {
        self.send_command(Command::PortSubscribe(port_steal)).await
    }

    /// Handles the conversion of [`LayerTcpSteal::PortUnsubscribe`], that is passed from the
    /// agent, to an internal stealer command [`Command::PortUnsubscribe`].
    ///
    /// The actual handling of this message is done in [`TcpConnectionStealer`].
    pub(crate) async fn port_unsubscribe(&mut self, port: Port) -> AgentResult<()> {
        self.send_command(Command::PortUnsubscribe(port)).await
    }

    /// Handles the conversion of [`LayerTcpSteal::ConnectionUnsubscribe`], that is passed from the
    /// agent, to an internal stealer command [`Command::ConnectionUnsubscribe`].
    ///
    /// The actual handling of this message is done in [`TcpConnectionStealer`].
    pub(crate) async fn connection_unsubscribe(
        &mut self,
        connection_id: ConnectionId,
    ) -> AgentResult<()> {
        self.send_command(Command::ConnectionUnsubscribe(connection_id))
            .await
    }

    /// Handles the conversion of [`TcpData`], that is passed from the
    /// agent, to an internal stealer command [`Command::ResponseData`].
    ///
    /// The actual handling of this message is done in [`TcpConnectionStealer`].
    pub(crate) async fn client_data(&mut self, tcp_data: TcpData) -> AgentResult<()> {
        self.send_command(Command::ResponseData(tcp_data)).await
    }

    /// Handles the conversion of [`LayerTcpSteal::HttpResponse`], that is passed from the
    /// agent, to an internal stealer command [`Command::HttpResponse`].
    ///
    /// The actual handling of this message is done in [`TcpConnectionStealer`].
    pub(crate) async fn http_response(
        &mut self,
        response: HttpResponseFallback,
    ) -> AgentResult<()> {
        self.send_command(Command::HttpResponse(response)).await
    }

    pub(crate) async fn switch_protocol_version(
        &mut self,
        version: semver::Version,
    ) -> AgentResult<()> {
        self.send_command(Command::SwitchProtocolVersion(version))
            .await
    }

    pub(crate) async fn handle_client_message(
        &mut self,
        message: LayerTcpSteal,
    ) -> AgentResult<()> {
        use std::sync::atomic::Ordering;

        match message {
            LayerTcpSteal::PortSubscribe(port_steal) => self.port_subscribe(port_steal).await,
            LayerTcpSteal::ConnectionUnsubscribe(connection_id) => {
                let all_in_progress = self.response_body_txs.len();
                self.response_body_txs
                    .retain(|(key_id, _), _| *key_id != connection_id);
                HTTP_REQUEST_IN_PROGRESS_COUNT.fetch_sub(
                    (all_in_progress - self.response_body_txs.len()) as i64,
                    Ordering::Relaxed,
                );

                self.connection_unsubscribe(connection_id).await
            }
            LayerTcpSteal::PortUnsubscribe(port) => self.port_unsubscribe(port).await,
            LayerTcpSteal::Data(tcp_data) => self.client_data(tcp_data).await,
            LayerTcpSteal::HttpResponse(response) => {
                self.http_response(HttpResponseFallback::Fallback(response))
                    .await
            }
            LayerTcpSteal::HttpResponseFramed(response) => {
                self.http_response(HttpResponseFallback::Framed(response))
                    .await
            }
            LayerTcpSteal::HttpResponseChunked(inner) => match inner {
                ChunkedResponse::Start(response) => {
                    let (tx, rx) = mpsc::channel(12);
                    let body = ReceiverStreamBody::new(ReceiverStream::from(rx));
                    let http_response: HttpResponse<ReceiverStreamBody> = HttpResponse {
                        port: response.port,
                        connection_id: response.connection_id,
                        request_id: response.request_id,
                        internal_response: InternalHttpResponse {
                            status: response.internal_response.status,
                            version: response.internal_response.version,
                            headers: response.internal_response.headers,
                            body,
                        },
                    };

                    let key = (response.connection_id, response.request_id);
                    self.response_body_txs.insert(key, tx.clone());
                    // HTTP_REQUEST_IN_PROGRESS_COUNT
                    //     .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                    self.http_response(HttpResponseFallback::Streamed(http_response))
                        .await?;

                    for frame in response.internal_response.body {
                        if let Err(err) = tx.send(Ok(frame.into())).await {
                            self.response_body_txs.remove(&key);
                            HTTP_REQUEST_IN_PROGRESS_COUNT.fetch_sub(1, Ordering::Relaxed);
                            tracing::trace!(?err, "error while sending streaming response frame");
                        }
                    }
                    Ok(())
                }
                ChunkedResponse::Body(body) => {
                    let key = &(body.connection_id, body.request_id);
                    let mut send_err = false;
                    if let Some(tx) = self.response_body_txs.get(key) {
                        for frame in body.frames {
                            if let Err(err) = tx.send(Ok(frame.into())).await {
                                send_err = true;
                                tracing::trace!(
                                    ?err,
                                    "error while sending streaming response body"
                                );
                                break;
                            }
                        }
                    }
                    if send_err || body.is_last {
                        self.response_body_txs.remove(key);
                        HTTP_REQUEST_IN_PROGRESS_COUNT.fetch_sub(1, Ordering::Relaxed);
                    };
                    Ok(())
                }
                ChunkedResponse::Error(err) => {
                    self.response_body_txs
                        .remove(&(err.connection_id, err.request_id));
                    HTTP_REQUEST_IN_PROGRESS_COUNT.fetch_sub(1, Ordering::Relaxed);
                    tracing::trace!(?err, "ChunkedResponse error received");
                    Ok(())
                }
            },
        }
    }
}
