use std::{
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use futures::{Stream, StreamExt};
use hyper::body::Frame;
use mirrord_protocol::tcp::InternalHttpBodyFrame;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc,
};
use tokio_stream::wrappers::{errors::BroadcastStreamRecvError, BroadcastStream};

use super::{error::HttpDetectError, ConnError, Redirected};
use crate::{
    http::HttpVersion,
    steal::tls::{self, handler::PassThroughTlsConnector, StealTlsHandlerStore},
};

pub mod http;
pub mod tcp;
mod util;

pub trait IncomingIO: 'static + AsyncRead + AsyncWrite + Send + Sync + Unpin {}

impl<T> IncomingIO for T where T: 'static + AsyncRead + AsyncWrite + Send + Sync + Unpin {}

pub struct MaybeHttp {
    pub info: ConnectionInfo,
    pub http_version: Option<HttpVersion>,
    pub stream: Box<dyn IncomingIO>,
}

impl MaybeHttp {
    /// Timeout for detemining if the redirected connection is HTTP.
    const HTTP_DETECTION_TIMEOUT: Duration = Duration::from_secs(10);

    pub async fn initialize(
        redirected: Redirected,
        tls_handlers: &StealTlsHandlerStore,
    ) -> Result<Self, HttpDetectError> {
        let original_destination = redirected.destination;
        let peer_addr = redirected.source;
        let local_addr = redirected
            .stream
            .local_addr()
            .map_err(HttpDetectError::LocalAddr)?;
        let tls_handler = tls_handlers.get(original_destination.port()).await?;

        let Some(tls_handler) = tls_handler else {
            let (stream, http_version) =
                crate::http::detect_http_version(redirected.stream, Self::HTTP_DETECTION_TIMEOUT)
                    .await
                    .map_err(HttpDetectError::HttpDetect)?;

            return Ok(Self {
                stream: Box::new(stream),
                http_version,
                info: ConnectionInfo {
                    original_destination,
                    local_addr,
                    peer_addr,
                    tls_connector: None,
                },
            });
        };

        let stream = tls_handler
            .acceptor()
            .accept(redirected.stream)
            .await
            .map_err(HttpDetectError::TlsAccept)?;
        let tls_connector = tls_handler.connector(stream.get_ref().1);

        let (stream, http_version): (Box<dyn IncomingIO>, _) = match tls_connector.alpn_protocol() {
            Some(tls::HTTP_2_ALPN_NAME) => (Box::new(stream), Some(HttpVersion::V2)),
            Some(tls::HTTP_1_1_ALPN_NAME) => (Box::new(stream), Some(HttpVersion::V1)),
            Some(tls::HTTP_1_0_ALPN_NAME) => (Box::new(stream), Some(HttpVersion::V1)),
            Some(..) => (Box::new(stream), None),
            None => {
                let (stream, http_version) =
                    crate::http::detect_http_version(stream, Self::HTTP_DETECTION_TIMEOUT)
                        .await
                        .map_err(HttpDetectError::HttpDetect)?;
                (Box::new(stream), http_version)
            }
        };

        Ok(Self {
            stream,
            http_version,
            info: ConnectionInfo {
                original_destination,
                local_addr,
                peer_addr,
                tls_connector: Some(tls_connector),
            },
        })
    }
}

#[derive(Clone, Debug)]
pub struct ConnectionInfo {
    pub original_destination: SocketAddr,
    pub local_addr: SocketAddr,
    pub peer_addr: SocketAddr,
    pub tls_connector: Option<PassThroughTlsConnector>,
}

impl ConnectionInfo {
    pub fn pass_through_address(&self) -> SocketAddr {
        let localhost = if self.original_destination.is_ipv4() {
            Ipv4Addr::LOCALHOST.into()
        } else {
            Ipv6Addr::LOCALHOST.into()
        };

        SocketAddr::new(localhost, self.original_destination.port())
    }
}

pub enum IncomingStream {
    Mpsc(mpsc::Receiver<IncomingStreamItem>),
    Broadcast(BroadcastStream<IncomingStreamItem>),
    Exhausted,
}

impl Stream for IncomingStream {
    type Item = IncomingStreamItem;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        let item = match this {
            Self::Mpsc(rx) => std::task::ready!(rx.poll_recv(cx))
                .unwrap_or(IncomingStreamItem::Finished(Err(ConnError::Panicked))),
            Self::Broadcast(rx) => match std::task::ready!(rx.poll_next_unpin(cx)) {
                Some(Ok(item)) => item,
                Some(Err(BroadcastStreamRecvError::Lagged(..))) => {
                    IncomingStreamItem::Finished(Err(ConnError::Lagged))
                }
                None => IncomingStreamItem::Finished(Err(ConnError::Panicked)),
            },
            Self::Exhausted => return Poll::Ready(None),
        };

        if matches!(item, IncomingStreamItem::Finished(..)) {
            *this = Self::Exhausted;
        }

        Poll::Ready(Some(item))
    }
}

#[derive(Clone)]
pub enum IncomingStreamItem {
    Frame(InternalHttpBodyFrame),
    NoMoreFrames,
    Data(Vec<u8>),
    NoMoreData,
    Finished(Result<(), ConnError>),
}

impl From<&[u8]> for IncomingStreamItem {
    fn from(data: &[u8]) -> Self {
        Self::Data(data.to_vec())
    }
}

impl From<&Frame<Bytes>> for IncomingStreamItem {
    fn from(frame: &Frame<Bytes>) -> Self {
        Self::Frame(InternalHttpBodyFrame::from(frame))
    }
}
