use std::{
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
        connection::OperatorConnection,
        error::{OperatorApiError, OperatorOperation},
    },
    types::{RECONNECT_NOT_POSSIBLE_CODE, RECONNECT_NOT_POSSIBLE_REASON},
};
use mirrord_protocol::{ClientCodec, ClientMessage, DaemonMessage};
use mirrord_protocol_api::client::{ClientConfig, ClientError, MirrordClient, ProtocolConnector};
use mirrord_protocol_io::{
    Client,
    websocket::{WebSocketChannel, WebSocketConnectionError},
};
use mirrord_sessions_manager_client::{
    IntproxyClient, SessionsManagerClientError, SessionsManagerConnectInfo,
};
use tokio::{io::DuplexStream, net::TcpStream};
use tokio_tungstenite::MaybeTlsStream;
use tokio_util::codec::Encoder;

#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    #[error(transparent)]
    Operator(<OperatorConnection as Sink<ClientMessage>>::Error),

    #[error(transparent)]
    Direct(std::io::Error),

    #[error(transparent)]
    Kube(#[from] kube::Error),

    #[error(transparent)]
    OperatorApi(#[from] OperatorApiError),

    #[error(transparent)]
    SessionsManagerConnect(#[from] SessionsManagerClientError),

    #[error(transparent)]
    SessionsManager(WebSocketConnectionError),

    #[error("failed to encode message for sessions-manager data plane: {0}")]
    SessionsManagerEncode(#[from] bincode::error::EncodeError),
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
#[derive(Debug)]
pub(crate) struct SessionsManagerConnector {
    pub(crate) connect_info: SessionsManagerConnectInfo,
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
    SessionsManager(WebSocketChannel<MaybeTlsStream<TcpStream>, Client>),
}

impl Sink<ClientMessage> for AgentConnection {
    type Error = ConnectionError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.get_mut() {
            Self::Operator(conn) => {
                <OperatorConnection as SinkExt<ClientMessage>>::poll_ready_unpin(conn, cx)
                    .map_err(ConnectionError::Operator)
            }
            Self::Direct(framed) => {
                <Framed as SinkExt<ClientMessage>>::poll_ready_unpin(framed, cx)
                    .map_err(ConnectionError::Direct)
            }
            Self::SessionsManager(conn) => {
                <WebSocketChannel<_, Client> as SinkExt<Vec<u8>>>::poll_ready_unpin(conn, cx)
                    .map_err(ConnectionError::SessionsManager)
            }
        }
    }

    fn start_send(self: Pin<&mut Self>, item: ClientMessage) -> Result<(), Self::Error> {
        match self.get_mut() {
            Self::Operator(operator_connection) => {
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
            Self::SessionsManager(conn) => {
                let bytes = bincode::encode_to_vec(&item, bincode::config::standard())?;
                <WebSocketChannel<_, Client> as SinkExt<Vec<u8>>>::start_send_unpin(conn, bytes)
                    .map_err(ConnectionError::SessionsManager)
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.get_mut() {
            Self::Operator(conn) => {
                <OperatorConnection as SinkExt<ClientMessage>>::poll_flush_unpin(conn, cx)
                    .map_err(ConnectionError::Operator)
            }
            Self::Direct(framed) => {
                <Framed as SinkExt<ClientMessage>>::poll_flush_unpin(framed, cx)
                    .map_err(ConnectionError::Direct)
            }
            Self::SessionsManager(conn) => {
                <WebSocketChannel<_, Client> as SinkExt<Vec<u8>>>::poll_flush_unpin(conn, cx)
                    .map_err(ConnectionError::SessionsManager)
            }
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.get_mut() {
            Self::Operator(conn) => {
                <OperatorConnection as SinkExt<ClientMessage>>::poll_close_unpin(conn, cx)
                    .map_err(ConnectionError::Operator)
            }
            Self::Direct(framed) => {
                <Framed as SinkExt<ClientMessage>>::poll_close_unpin(framed, cx)
                    .map_err(ConnectionError::Direct)
            }
            Self::SessionsManager(conn) => {
                <WebSocketChannel<_, Client> as SinkExt<Vec<u8>>>::poll_close_unpin(conn, cx)
                    .map_err(ConnectionError::SessionsManager)
            }
        }
    }
}

impl Sink<Vec<u8>> for AgentConnection {
    type Error = ConnectionError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.get_mut() {
            Self::Operator(conn) => {
                <OperatorConnection as SinkExt<Vec<u8>>>::poll_ready_unpin(conn, cx)
                    .map_err(ConnectionError::Operator)
            }
            Self::Direct(framed) => <Framed as SinkExt<Vec<u8>>>::poll_ready_unpin(framed, cx)
                .map_err(ConnectionError::Direct),
            Self::SessionsManager(conn) => {
                <WebSocketChannel<_, Client> as SinkExt<Vec<u8>>>::poll_ready_unpin(conn, cx)
                    .map_err(ConnectionError::SessionsManager)
            }
        }
    }

    fn start_send(self: Pin<&mut Self>, item: Vec<u8>) -> Result<(), Self::Error> {
        match self.get_mut() {
            Self::Operator(operator_connection) => {
                <OperatorConnection as SinkExt<Vec<u8>>>::start_send_unpin(
                    operator_connection,
                    item,
                )
                .map_err(ConnectionError::Operator)
            }
            Self::Direct(framed) => <Framed as SinkExt<Vec<u8>>>::start_send_unpin(framed, item)
                .map_err(ConnectionError::Direct),
            Self::SessionsManager(conn) => {
                <WebSocketChannel<_, Client> as SinkExt<Vec<u8>>>::start_send_unpin(conn, item)
                    .map_err(ConnectionError::SessionsManager)
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.get_mut() {
            Self::Operator(conn) => {
                <OperatorConnection as SinkExt<Vec<u8>>>::poll_flush_unpin(conn, cx)
                    .map_err(ConnectionError::Operator)
            }
            Self::Direct(framed) => <Framed as SinkExt<Vec<u8>>>::poll_flush_unpin(framed, cx)
                .map_err(ConnectionError::Direct),
            Self::SessionsManager(conn) => {
                <WebSocketChannel<_, Client> as SinkExt<Vec<u8>>>::poll_flush_unpin(conn, cx)
                    .map_err(ConnectionError::SessionsManager)
            }
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.get_mut() {
            Self::Operator(conn) => {
                <OperatorConnection as SinkExt<Vec<u8>>>::poll_close_unpin(conn, cx)
                    .map_err(ConnectionError::Operator)
            }
            Self::Direct(framed) => <Framed as SinkExt<Vec<u8>>>::poll_close_unpin(framed, cx)
                .map_err(ConnectionError::Direct),
            Self::SessionsManager(conn) => {
                <WebSocketChannel<_, Client> as SinkExt<Vec<u8>>>::poll_close_unpin(conn, cx)
                    .map_err(ConnectionError::SessionsManager)
            }
        }
    }
}

impl Stream for AgentConnection {
    type Item = Result<DaemonMessage, ConnectionError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.get_mut() {
            AgentConnection::Operator(conn) => {
                conn.poll_next_unpin(cx).map_err(ConnectionError::Operator)
            }
            AgentConnection::Direct(framed) => {
                framed.poll_next_unpin(cx).map_err(ConnectionError::Direct)
            }
            AgentConnection::SessionsManager(conn) => conn
                .poll_next_unpin(cx)
                .map_err(ConnectionError::SessionsManager),
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
                    .await?
                    .take_stream(direct.info.agent_port)
                    .expect("agent port should've been portforwarded");

                Ok(AgentConnection::Direct(Framed::new(stream, Codec)))
            }
            AgentConnector::SessionsManager(sessions_manager) => {
                let client = IntproxyClient::new(sessions_manager.connect_info.clone(), None)?;
                let conn = client.connect_raw(Duration::from_mins(10)).await?;

                Ok(AgentConnection::SessionsManager(conn))
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
