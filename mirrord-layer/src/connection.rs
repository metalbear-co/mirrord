use std::{
    pin::Pin,
    task::{Context, Poll},
};

use actix_codec::{AsyncRead, AsyncWrite, ReadBuf};
use mirrord_config::LayerConfig;
use rand::Rng;
use tokio::net::TcpStream;
use tracing::log::info;

use crate::{error::LayerError, pod_api};

pub(crate) enum AgentConnection<T>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    TcpStream(TcpStream),
    Portforwarder(T),
}

impl<T> AsyncRead for AgentConnection<T>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::TcpStream(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Portforwarder(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl<T> AsyncWrite for AgentConnection<T>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Self::TcpStream(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Portforwarder(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), std::io::Error>> {
        match self.get_mut() {
            Self::TcpStream(stream) => Pin::new(stream).poll_flush(cx),
            Self::Portforwarder(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), std::io::Error>> {
        match self.get_mut() {
            Self::TcpStream(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Portforwarder(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

pub(crate) async fn connect(config: &LayerConfig) -> impl AsyncWrite + AsyncRead + Unpin {
    if let Some(address) = &config.connect_tcp {
        let stream = TcpStream::connect(address)
            .await
            .unwrap_or_else(|_| panic!("Failed to connect to TCP socket {address:?}"));
        AgentConnection::TcpStream(stream)
    } else {
        let connection_port: u16 = rand::thread_rng().gen_range(30000..=65535);
        info!("Using port `{connection_port:?}` for communication");
        let mut port_forwarder = pod_api::create_agent(config.clone(), connection_port).await.unwrap_or_else(|err| match err {
            LayerError::KubeError(kube::Error::HyperError(err)) => {
                eprintln!("\nmirrord encountered an error accessing the Kubernetes API. Consider passing --accept-invalid-certificates.\n");

                match err.into_cause() {
                    Some(cause) => panic!("{}", cause),
                    None => panic!("mirrord got KubeError::HyperError"),
                }
            }
            _ => panic!("failed to create agent in k8s: {}", err),
        });
        AgentConnection::Portforwarder(port_forwarder.take_stream(connection_port).unwrap())
    }
}
