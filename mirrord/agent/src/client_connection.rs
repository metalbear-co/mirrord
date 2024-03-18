use std::{fmt, io};

use actix_codec::Framed;
use futures::{SinkExt, TryStreamExt};
use mirrord_protocol::{ClientMessage, DaemonCodec, DaemonMessage};
use tokio::net::TcpStream;
use tokio_rustls::{client::TlsStream, rustls::pki_types::ServerName, TlsConnector};

use crate::util::ClientId;

/// Wrapper over client's network connection with the agent.
pub struct ClientConnection {
    framed: ConnectionFramed,
    client_id: ClientId,
}

impl ClientConnection {
    /// Wraps the given [`TcpStream`] into this struct.
    /// If a [`TlsConnector`] is given, it is used to first build a make a TLS connection using the
    /// given [`TcpStream`].
    #[tracing::instrument(level = "trace", skip(tls), fields(use_tls = tls.is_some()), err)]
    pub async fn new(
        stream: TcpStream,
        client_id: u32,
        tls: Option<TlsConnector>,
    ) -> io::Result<Self> {
        let framed = match tls {
            Some(connector) => {
                let peer_ip = stream.peer_addr()?.ip();
                let tls_stream = connector
                    .connect(ServerName::IpAddress(peer_ip.into()), stream)
                    .await?;

                ConnectionFramed::Tls(Framed::new(tls_stream, DaemonCodec::default()))
            }
            None => ConnectionFramed::Tcp(Framed::new(stream, DaemonCodec::default())),
        };

        Ok(Self { framed, client_id })
    }

    /// Sends a [`DaemonMessage`] to the client.
    #[tracing::instrument(level = "trace", err)]
    pub async fn send(&mut self, message: DaemonMessage) -> io::Result<()> {
        match &mut self.framed {
            ConnectionFramed::Tcp(framed) => framed.send(message).await?,
            ConnectionFramed::Tls(framed) => framed.send(message).await?,
        }

        Ok(())
    }

    /// Receives a [`ClientMessage`] from the client.
    #[tracing::instrument(level = "trace", err)]
    pub async fn receive(&mut self) -> io::Result<Option<ClientMessage>> {
        match &mut self.framed {
            ConnectionFramed::Tcp(framed) => framed.try_next().await,
            ConnectionFramed::Tls(framed) => framed.try_next().await,
        }
    }
}

impl fmt::Debug for ClientConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientConnection")
            .field("client_id", &self.client_id)
            .field(
                "uses_tls",
                &matches!(self.framed, ConnectionFramed::Tls(..)),
            )
            .finish()
    }
}

/// Enum wraps whole [`Framed`] instead of just [`TcpStream`]/[`TlsStream`], so we don't have to
/// implement [`AsyncRead`](actix_codec::AsyncRead) and [`AsyncWrite`](actix_codec::AsyncWrite).
enum ConnectionFramed {
    Tcp(Framed<TcpStream, DaemonCodec>),
    Tls(Framed<TlsStream<TcpStream>, DaemonCodec>),
}
