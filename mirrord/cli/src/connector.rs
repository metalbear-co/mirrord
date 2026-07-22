use std::{
    num::NonZeroUsize,
    pin::Pin,
    task::{Context, Poll},
};

use actix_codec::Decoder;
use futures::{Sink, SinkExt, Stream, StreamExt};
use k8s_openapi::api::core::v1::Pod;
use kube::Api;
use mirrord_kube::api::kubernetes::AgentKubernetesConnectInfo;
use mirrord_operator::client::{
    OperatorApi, OperatorSession, PreparedClientCert, connection::OperatorConnection,
    error::OperatorApiError,
};
use mirrord_progress::{Progress, ProgressTracker};
use mirrord_protocol::{ClientCodec, ClientMessage, DaemonMessage};
use mirrord_protocol_api::client::{ClientConfig, ClientError, MirrordClient, ProtocolConnector};
use tokio::io::DuplexStream;
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
}

/// Provides `mirrord-protocol` connections to a
/// [`MirrordClient`](mirrord_protocol_api::client::MirrordClient), either through the
/// mirrord-operator or by port-forwarding directly to an agent pod.
///
/// Reconnecting is supported: the operator variant reconnects to its existing session, and the
/// direct variant re-establishes the port-forward.
#[derive(Debug)]
pub(crate) enum AgentConnector {
    Operator(OperatorConnector),
    Direct(DirectConnector),
}

impl AgentConnector {
    pub async fn into_client(
        self,
        progress: &mut ProgressTracker,
    ) -> Result<MirrordClient, ClientError> {
        MirrordClient::new(
            self,
            ClientConfig::cli(),
            NonZeroUsize::new(1024).expect("channel size is nonzero"),
            progress,
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
    pub(crate) api: OperatorApi<PreparedClientCert>,
    pub(crate) session: Box<OperatorSession>,
    pub(crate) first_conn: Option<OperatorConnection>,
}

#[derive(Debug)]
pub(crate) struct DirectConnector {
    pub(crate) api: Api<Pod>,
    pub(crate) info: AgentKubernetesConnectInfo,
}

struct Codec;

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
    Operator(OperatorConnection),
    Direct(Framed),
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
        }
    }
}

impl ProtocolConnector for AgentConnector {
    type Error = ConnectionError;
    type Conn = AgentConnection;

    async fn connect<P: Progress>(&mut self, _progress: &mut P) -> Result<Self::Conn, Self::Error> {
        match self {
            AgentConnector::Operator(operator) => match operator.first_conn.take() {
                Some(conn) => Ok(AgentConnection::Operator(conn)),
                None => Ok(AgentConnection::Operator(
                    operator.api.connect_to_session(&operator.session).await?,
                )),
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
        }
    }

    fn can_reconnect(&self) -> bool {
        match self {
            AgentConnector::Operator(operator) => operator.session.allow_reconnect,
            AgentConnector::Direct(_) => true,
        }
    }
}
