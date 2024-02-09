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

use super::http_interceptor::{HttpInterceptorError, Result};

pub struct UpgradedConnection {
    pub stream: TcpStream,
    pub unprocessed_bytes: Bytes,
}

/// Handles the differences between hyper's HTTP/1 and HTTP/2 connections.
pub enum HttpSender {
    V1(http1::SendRequest<BoxBody<Bytes, Infallible>>),
    V2(http2::SendRequest<BoxBody<Bytes, Infallible>>),
}

pub async fn handshake(
    version: Version,
    target_stream: TcpStream,
) -> Result<(HttpSender, Receiver<Result<UpgradedConnection>>)> {
    match version {
        Version::HTTP_2 => {
            let (sender, connection) =
                http2::handshake(TokioExecutor::default(), TokioIo::new(target_stream)).await?;
            tokio::spawn(connection);

            let (upgrade_tx, upgrade_rx) = oneshot::channel();
            let _ = upgrade_tx.send(Err(HttpInterceptorError::UpgradeNotSupported(version)));

            Ok((HttpSender::V2(sender), upgrade_rx))
        }

        Version::HTTP_3 => Err(HttpInterceptorError::UnsupportedHttpVersion(version)),

        _http_v1 => {
            let (sender, mut connection) = http1::handshake(TokioIo::new(target_stream)).await?;

            let (upgrade_tx, upgrade_rx) = oneshot::channel::<Result<UpgradedConnection>>();

            tokio::spawn(async move {
                let res = future::poll_fn(|ctx| connection.poll_without_shutdown(ctx))
                    .await
                    .map_err(Into::into)
                    .map(|_| connection.into_parts())
                    .map(|Parts { io, read_buf, .. }: Parts<_>| UpgradedConnection {
                        stream: io.into_inner(),
                        unprocessed_bytes: read_buf,
                    });

                let _ = upgrade_tx.send(res);
            });

            Ok((HttpSender::V1(sender), upgrade_rx))
        }
    }
}

impl HttpSender {
    pub async fn send(
        &mut self,
        req: HttpRequestFallback,
    ) -> Result<Response<Incoming>, HttpInterceptorError> {
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
