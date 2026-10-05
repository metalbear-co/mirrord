use std::{
    fmt,
    num::NonZeroUsize,
    ops::Not,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use actix_codec::Decoder;
use futures::{Sink, SinkExt, Stream, StreamExt};
use k8s_openapi::api::core::v1::Pod;
use kube::Api;
use mirrord_kube::api::kubernetes::AgentKubernetesConnectInfo;
use mirrord_operator::{
    client::{
        OperatorApi, OperatorSession, PreparedClientCert,
        error::{OperatorApiError, OperatorOperation},
    },
    types::{RECONNECT_NOT_POSSIBLE_CODE, RECONNECT_NOT_POSSIBLE_REASON},
};
use mirrord_operator_websocket::connection::OperatorConnection;
use mirrord_protocol::{ClientCodec, ClientMessage, DaemonMessage};
use mirrord_protocol_api::client::{ClientConfig, ClientError, MirrordClient, ProtocolConnector};
use mirrord_sessions_manager_client::{
    DirectTransport, IntproxyClient, OperatorTransport, SessionsManagerClientError,
    SessionsManagerConnectInfo, SessionsManagerTransport,
};
use tokio::io::DuplexStream;
use tokio_util::codec::Encoder;

#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    #[error(transparent)]
    Operator(<OperatorConnection as Sink<ClientMessage>>::Error),

    #[error(transparent)]
    Direct(std::io::Error),

    /// The WebSocket upgrade request that opens the port-forward to the agent pod failed.
    ///
    /// Some callers keep only the message (`to_string`), so the message includes the
    /// [`kube::Error`], which has the HTTP status and the response body. The error is not also a
    /// `#[source]`, because then miette would show it two times.
    #[error("agent port-forward WebSocket upgrade failed: {0}")]
    AgentPortForward(kube::Error),

    #[error(transparent)]
    OperatorApi(#[from] OperatorApiError),

    #[error(transparent)]
    SessionsManagerConnect(#[from] SessionsManagerClientError),
}

/// Provides `mirrord-protocol` connections to a [`MirrordClient`],
/// either through the mirrord-operator or by port-forwarding directly
/// to an agent pod.
///
/// Reconnecting is supported: the operator variant reconnects to its existing session, and the
/// direct variant re-establishes the port-forward.
#[derive(Debug)]
pub(crate) enum AgentConnector {
    Operator(OperatorConnector),
    Direct(DirectConnector),
    SessionsManager(SessionsManagerConnector),
}

impl AgentConnector {
    pub async fn into_client(self) -> Result<MirrordClient, ClientError> {
        MirrordClient::new(
            self,
            ClientConfig::cli(),
            NonZeroUsize::new(16).expect("channel size is nonzero"),
        )
        .await
    }
}

/// Connects to an operator session that was prepared during setup.
///
/// The first connection is established while the session is set up and parked in
/// [`Self::first_conn`], to be handed out on the first [`connect`](AgentConnector::connect) call.
/// Reconnects go through [`OperatorApi::connect_to_session`], reusing [`Self::session`].
#[derive(Debug)]
pub(crate) struct OperatorConnector {
    pub(crate) api: Box<OperatorApi<PreparedClientCert>>,
    pub(crate) session: Box<OperatorSession>,
    pub(crate) first_conn: Option<Box<OperatorConnection>>,
    pub(crate) failed: bool,
}

impl OperatorConnector {
    /// Handle errors from [`OperatorApi`].
    ///
    /// Sets [`Self::failed`] when the error implies we can no longer
    /// reconnect.
    fn handle_error(&mut self, error: &OperatorApiError) {
        if let OperatorApiError::KubeError {
            error: kube::Error::Api(error),
            operation: OperatorOperation::WebsocketConnection,
        } = error
        {
            self.failed |= error.code == RECONNECT_NOT_POSSIBLE_CODE
                && error.reason == RECONNECT_NOT_POSSIBLE_REASON;
        }
    }

    fn can_reconnect(&self) -> bool {
        self.session.allow_reconnect && self.failed.not()
    }
}

#[derive(Debug)]
pub(crate) struct DirectConnector {
    pub(crate) api: Api<Pod>,
    pub(crate) info: AgentKubernetesConnectInfo,
}

/// Connects to an agent through a mirrord-sessions-manager room.
///
/// Each [`connect`](AgentConnector::connect) call opens a fresh, one-shot data plane connection
/// to the room. There is no reconnect support: once a connection to the room fails, the session
/// is over, same as [`DirectConnector`].
pub(crate) struct SessionsManagerConnector {
    pub(crate) connect_info: SessionsManagerConnectInfo,
    /// Set when the sessions-manager is hosted by the operator and reached through
    /// kube-apiserver; otherwise it's the standalone one configured by the environment.
    pub(crate) operator_client: Option<kube::Client>,
}

impl SessionsManagerConnector {
    async fn connect(&self) -> Result<OperatorConnection, SessionsManagerClientError> {
        match &self.operator_client {
            Some(client) => {
                self.connect_with(OperatorTransport::new(client.clone()))
                    .await
            }
            None => self.connect_with(DirectTransport::from_env()?).await,
        }
    }

    /// Generic over the transport because [`SessionsManagerTransport`] isn't object-safe.
    async fn connect_with<T: SessionsManagerTransport>(
        &self,
        transport: T,
    ) -> Result<OperatorConnection, SessionsManagerClientError> {
        let client = IntproxyClient::with_transport(self.connect_info.clone(), transport)?;
        Box::pin(client.connect(Duration::from_mins(10))).await
    }
}

impl fmt::Debug for SessionsManagerConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionsManagerConnector")
            .field("connect_info", &self.connect_info)
            .field("operator_hosted", &self.operator_client.is_some())
            .finish()
    }
}

pub struct Codec;

impl Encoder<ClientMessage> for Codec {
    type Error = std::io::Error;

    fn encode(
        &mut self,
        item: ClientMessage,
        dst: &mut bytes::BytesMut,
    ) -> Result<(), Self::Error> {
        ClientCodec::default().encode(item, dst)
    }
}

impl Encoder<Vec<u8>> for Codec {
    type Error = std::io::Error;

    fn encode(&mut self, item: Vec<u8>, dst: &mut bytes::BytesMut) -> Result<(), Self::Error> {
        dst.extend_from_slice(&item);
        Ok(())
    }
}

impl Decoder for Codec {
    type Item = DaemonMessage;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut bytes::BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        ClientCodec::default().decode(src)
    }
}

pub type Framed = tokio_util::codec::Framed<DuplexStream, Codec>;
pub enum AgentConnection {
    Operator(Box<OperatorConnection>),
    Direct(Framed),
    SessionsManager(Box<OperatorConnection>),
}

impl Sink<ClientMessage> for AgentConnection {
    type Error = ConnectionError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.get_mut() {
            Self::Operator(conn) | Self::SessionsManager(conn) => {
                <OperatorConnection as SinkExt<ClientMessage>>::poll_ready_unpin(conn, cx)
                    .map_err(ConnectionError::Operator)
            }
            Self::Direct(framed) => {
                <Framed as SinkExt<ClientMessage>>::poll_ready_unpin(framed, cx)
                    .map_err(ConnectionError::Direct)
            }
        }
    }

    fn start_send(self: Pin<&mut Self>, item: ClientMessage) -> Result<(), Self::Error> {
        match self.get_mut() {
            Self::Operator(operator_connection) | Self::SessionsManager(operator_connection) => {
                <OperatorConnection as SinkExt<ClientMessage>>::start_send_unpin(
                    operator_connection,
                    item,
                )
                .map_err(ConnectionError::Operator)
            }
            Self::Direct(framed) => {
                <Framed as SinkExt<ClientMessage>>::start_send_unpin(framed, item)
                    .map_err(ConnectionError::Direct)
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.get_mut() {
            Self::Operator(conn) | Self::SessionsManager(conn) => {
                <OperatorConnection as SinkExt<ClientMessage>>::poll_flush_unpin(conn, cx)
                    .map_err(ConnectionError::Operator)
            }
            Self::Direct(framed) => {
                <Framed as SinkExt<ClientMessage>>::poll_flush_unpin(framed, cx)
                    .map_err(ConnectionError::Direct)
            }
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.get_mut() {
            Self::Operator(conn) | Self::SessionsManager(conn) => {
                <OperatorConnection as SinkExt<ClientMessage>>::poll_close_unpin(conn, cx)
                    .map_err(ConnectionError::Operator)
            }
            Self::Direct(framed) => {
                <Framed as SinkExt<ClientMessage>>::poll_close_unpin(framed, cx)
                    .map_err(ConnectionError::Direct)
            }
        }
    }
}

impl Sink<Vec<u8>> for AgentConnection {
    type Error = ConnectionError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.get_mut() {
            Self::Operator(conn) | Self::SessionsManager(conn) => {
                <OperatorConnection as SinkExt<Vec<u8>>>::poll_ready_unpin(conn, cx)
                    .map_err(ConnectionError::Operator)
            }
            Self::Direct(framed) => <Framed as SinkExt<Vec<u8>>>::poll_ready_unpin(framed, cx)
                .map_err(ConnectionError::Direct),
        }
    }

    fn start_send(self: Pin<&mut Self>, item: Vec<u8>) -> Result<(), Self::Error> {
        match self.get_mut() {
            Self::Operator(operator_connection) | Self::SessionsManager(operator_connection) => {
                <OperatorConnection as SinkExt<Vec<u8>>>::start_send_unpin(
                    operator_connection,
                    item,
                )
                .map_err(ConnectionError::Operator)
            }
            Self::Direct(framed) => <Framed as SinkExt<Vec<u8>>>::start_send_unpin(framed, item)
                .map_err(ConnectionError::Direct),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.get_mut() {
            Self::Operator(conn) | Self::SessionsManager(conn) => {
                <OperatorConnection as SinkExt<Vec<u8>>>::poll_flush_unpin(conn, cx)
                    .map_err(ConnectionError::Operator)
            }
            Self::Direct(framed) => <Framed as SinkExt<Vec<u8>>>::poll_flush_unpin(framed, cx)
                .map_err(ConnectionError::Direct),
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.get_mut() {
            Self::Operator(conn) | Self::SessionsManager(conn) => {
                <OperatorConnection as SinkExt<Vec<u8>>>::poll_close_unpin(conn, cx)
                    .map_err(ConnectionError::Operator)
            }
            Self::Direct(framed) => <Framed as SinkExt<Vec<u8>>>::poll_close_unpin(framed, cx)
                .map_err(ConnectionError::Direct),
        }
    }
}

impl Stream for AgentConnection {
    type Item = Result<DaemonMessage, ConnectionError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.get_mut() {
            AgentConnection::Operator(conn) | AgentConnection::SessionsManager(conn) => {
                conn.poll_next_unpin(cx).map_err(ConnectionError::Operator)
            }
            AgentConnection::Direct(framed) => {
                framed.poll_next_unpin(cx).map_err(ConnectionError::Direct)
            }
        }
    }
}

impl ProtocolConnector for AgentConnector {
    type Error = ConnectionError;
    type Conn = AgentConnection;

    async fn connect(&mut self) -> Result<Self::Conn, Self::Error> {
        match self {
            AgentConnector::Operator(operator) => match operator.first_conn.take() {
                Some(conn) => Ok(AgentConnection::Operator(conn)),
                None => Ok(AgentConnection::Operator(Box::new(
                    operator
                        .api
                        .connect_to_session(&operator.session)
                        .await
                        .inspect_err(|err| operator.handle_error(err))?,
                ))),
            },
            AgentConnector::Direct(direct) => {
                let stream = direct
                    .api
                    .portforward(&direct.info.pod_name, &[direct.info.agent_port])
                    .await
                    .map_err(ConnectionError::AgentPortForward)?
                    .take_stream(direct.info.agent_port)
                    .expect("agent port should've been portforwarded");

                Ok(AgentConnection::Direct(Framed::new(stream, Codec)))
            }
            AgentConnector::SessionsManager(sessions_manager) => {
                let conn = sessions_manager.connect().await?;

                Ok(AgentConnection::SessionsManager(Box::new(conn)))
            }
        }
    }

    fn can_reconnect(&self) -> bool {
        match self {
            AgentConnector::Operator(operator) => operator.can_reconnect(),
            // Reconnects are only supported on operator.
            AgentConnector::Direct(_) => false,
            AgentConnector::SessionsManager(_) => false,
        }
    }
}

#[cfg(test)]
mod test {
    use http::{Request, Response, StatusCode};
    use kube::{Client, client::Body};

    use super::*;

    /// A proxy between mirrord and the API server can reject the port-forward upgrade with a
    /// response that is not a Kubernetes `Status`. The error message must name the failed step, and
    /// keep the HTTP status and the response body, because they are the only hints about what
    /// rejected it.
    #[tokio::test]
    async fn rejected_direct_upgrade_reports_status_and_body() {
        let (service, mut handle) = tower_test::mock::pair::<Request<Body>, Response<Body>>();
        let mut connector = AgentConnector::Direct(DirectConnector {
            api: Api::namespaced(Client::new(service, "default"), "default"),
            info: AgentKubernetesConnectInfo {
                pod_name: "mirrord-agent".to_owned(),
                pod_namespace: "default".to_owned(),
                agent_port: 44128,
            },
        });

        let (result, ()) = tokio::join!(connector.connect(), async {
            let (_, send) = handle.next_request().await.unwrap();
            send.send_response(
                Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::from(b"proxy rejected the upgrade".to_vec()))
                    .unwrap(),
            );
        });

        let Err(error) = result else {
            panic!("connect must fail when the upgrade is rejected");
        };
        let message = error.to_string();
        assert!(
            message.starts_with("agent port-forward WebSocket upgrade failed"),
            "{message}"
        );
        assert!(message.contains("500"), "{message}");
        assert!(message.contains("proxy rejected the upgrade"), "{message}");
    }
}
