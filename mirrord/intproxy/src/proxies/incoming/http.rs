use std::{convert::Infallible, future};

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use hyper::{
    body::Incoming,
    client::conn::{
        http1::{self, Parts},
        http2,
    },
    Response, Version,
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use mirrord_protocol::tcp::HttpRequestFallback;
use tokio::{
    net::TcpStream,
    sync::oneshot::{self, Receiver},
};

use super::interceptor::{Error, Result};

/// HTTP connection deconstructed after an `UPGRADE`.
pub struct TransportHandle {
    receiver: Option<Receiver<Result<(TcpStream, Bytes)>>>,
    version: Version,
}

impl TransportHandle {
    pub async fn reclaim(self) -> Result<(TcpStream, Bytes)> {
        self.receiver
            .ok_or(Error::UpgradeNotSupported(self.version))?
            .await
            .map_err(|_| Error::HttpConnectionTaskPanicked)?
    }
}

/// Handles the differences between hyper's HTTP/1 and HTTP/2 connections.
pub enum HttpSender {
    V1(http1::SendRequest<BoxBody<Bytes, Infallible>>),
    V2(http2::SendRequest<BoxBody<Bytes, Infallible>>),
}

/// Consumes the given [`TcpStream`] and performs an HTTP handshake, turning it into an HTTP
/// connection.
///
/// # Returns
///
/// * [`HttpSender`] that can be used to send HTTP requests to the peer.
/// * [`Receiver`] that can be used to reclaim the [`TcpStream`] after the HTTP connection ends with
///   an `UPGRADE`.
pub async fn handshake(
    version: Version,
    target_stream: TcpStream,
) -> Result<(HttpSender, TransportHandle)> {
    match version {
        Version::HTTP_2 => {
            let (sender, connection) =
                http2::handshake(TokioExecutor::default(), TokioIo::new(target_stream)).await?;
            tokio::spawn(connection);

            Ok((
                HttpSender::V2(sender),
                TransportHandle {
                    version,
                    receiver: None,
                },
            ))
        }

        Version::HTTP_3 => Err(Error::UnsupportedHttpVersion(version)),

        _http_v1 => {
            let (sender, mut connection) = http1::handshake(TokioIo::new(target_stream)).await?;

            let (upgrade_tx, upgrade_rx) = oneshot::channel::<Result<(TcpStream, Bytes)>>();

            tokio::spawn(async move {
                let res = future::poll_fn(|ctx| connection.poll_without_shutdown(ctx))
                    .await
                    .map_err(Into::into)
                    .map(|_| connection.into_parts())
                    .map(|Parts { io, read_buf, .. }: Parts<_>| (io.into_inner(), read_buf));

                let _ = upgrade_tx.send(res);
            });

            Ok((
                HttpSender::V1(sender),
                TransportHandle {
                    version,
                    receiver: Some(upgrade_rx),
                },
            ))
        }
    }
}

impl HttpSender {
    pub async fn send(&mut self, req: HttpRequestFallback) -> Result<Response<Incoming>, Error> {
        match self {
            Self::V1(sender) => {
                // Solves a "connection was not ready" client error.
                // https://rust-lang.github.io/wg-async/vision/submitted_stories/status_quo/barbara_tries_unix_socket.html#the-single-magical-line
                sender.ready().await?;
                sender
                    .send_request(req.into_hyper())
                    .await
                    .map_err(Into::into)
            }
            Self::V2(sender) => {
                // Solves a "connection was not ready" client error.
                // https://rust-lang.github.io/wg-async/vision/submitted_stories/status_quo/barbara_tries_unix_socket.html#the-single-magical-line
                sender.ready().await?;
                sender
                    .send_request(req.into_hyper())
                    .await
                    .map_err(Into::into)
            }
        }
    }
}
