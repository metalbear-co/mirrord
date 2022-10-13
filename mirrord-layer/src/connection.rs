use std::{
    pin::Pin,
    task::{Context, Poll},
};

use actix_codec::{AsyncRead, AsyncWrite, ReadBuf};
use mirrord_config::LayerConfig;
use rand::Rng;
use tokio::net::TcpStream;
use tracing::log::info;

use crate::{error::LayerError, pod_api::KubernetesAPI};

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

fn handle_error(err: LayerError) -> ! {
    match err {
        LayerError::KubeError(kube::Error::HyperError(err)) => {
            eprintln!("\nmirrord encountered an error accessing the Kubernetes API. Consider passing --accept-invalid-certificates.\n");

            match err.into_cause() {
                Some(cause) => panic!("{}", cause),
                None => panic!("mirrord got KubeError::HyperError"),
            }
        }
        _ => panic!("failed to create agent in k8s: {}", err),
    }
}

pub(crate) async fn connect(config: &LayerConfig) -> impl AsyncWrite + AsyncRead + Unpin {
    if let Some(address) = &config.connect_tcp {
        let stream = TcpStream::connect(address)
            .await
            .unwrap_or_else(|_| panic!("Failed to connect to TCP socket {address:?}"));
        AgentConnection::TcpStream(stream)
    } else {
        let k8s_api = KubernetesAPI::new(config).await.unwrap();
        let (pod_agent_name, agent_port) = {
            if let (Some(pod_agent_name), Some(agent_port)) =
                (&config.connect_agent_name, config.connect_agent_port)
            {
                info!(
                    "Reusing existing agent {:?}, port {:?}",
                    pod_agent_name, agent_port
                );
                (pod_agent_name.to_owned(), agent_port)
            } else {
                info!("No existing agent, spawning new one.");
                let agent_port: u16 = rand::thread_rng().gen_range(30000..=65535);
                info!("Using port `{agent_port:?}` for communication");
                let pod_agent_name = match k8s_api.create_agent(agent_port).await {
                    Ok(pod_name) => pod_name,
                    Err(err) => handle_error(err),
                };

                // Set env var for children to re-use.
                std::env::set_var("MIRRORD_CONNECT_AGENT", &pod_agent_name);
                std::env::set_var("MIRRORD_CONNECT_PORT", agent_port.to_string());
                // So children won't show progress as well as it might confuse users
                std::env::set_var(mirrord_progress::MIRRORD_PROGRESS_ENV, "off");
                (pod_agent_name, agent_port)
            }
        };

        let mut port_forwarder = match k8s_api.port_forward(&pod_agent_name, agent_port).await {
            Ok(port_forwarder) => port_forwarder,
            Err(err) => handle_error(err),
        };

        AgentConnection::Portforwarder(port_forwarder.take_stream(agent_port).unwrap())
    }
}
