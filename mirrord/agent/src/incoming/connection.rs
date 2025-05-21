use std::{
    fmt,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use futures::Stream;
use mirrord_protocol::tcp::InternalHttpBodyFrame;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc,
};

use super::{
    error::{ConnError, HttpDetectError},
    tls::{self, handler::PassThroughTlsConnector, StealTlsHandlerStore},
    Redirected,
};
use crate::http::HttpVersion;

pub mod http;
mod http_passthrough;
mod http_steal;
pub mod tcp;
mod tcp_task;
mod util;

/// Redirected connection info.
#[derive(Clone, Debug)]
pub struct ConnectionInfo {
    /// Original destination address of this connection.
    ///
    /// # Important
    ///
    /// Do not use it when passing this connection through,
    /// as we might hit an iptables loop.
    ///
    /// Use [`Self::pass_through_address`] instead.
    pub original_destination: SocketAddr,
    /// Address of the TCP listener that accepted this connection.
    pub local_addr: SocketAddr,
    /// Address of the TCP peer that made this connection.
    pub peer_addr: SocketAddr,
    /// TLS connector that should be used when passing this connection
    /// through to its original destination.
    pub tls_connector: Option<PassThroughTlsConnector>,
}

impl ConnectionInfo {
    /// Returns the address to use when passing this connection through.
    ///
    /// To avoid iptables loop, we always return localhost here.
    pub fn pass_through_address(&self) -> SocketAddr {
        let localhost = if self.original_destination.is_ipv4() {
            Ipv4Addr::LOCALHOST.into()
        } else {
            Ipv6Addr::LOCALHOST.into()
        };

        SocketAddr::new(localhost, self.original_destination.port())
    }
}

/// Supertrait for incoming IO streams.
///
/// [`MaybeHttp::detect`] transforms the incoming [`TcpStream`](tokio::net::TcpStream)
/// into one of multiple types due to possible TLS handshake and HTTP detection.
///
/// Having a super trait allows us to return [`Box<dyn IncomingStream>`]
/// and simplify the code in general.
pub trait IncomingIO: 'static + AsyncRead + AsyncWrite + Send + Sync + Unpin {}

impl<T> IncomingIO for T where T: 'static + AsyncRead + AsyncWrite + Send + Sync + Unpin {}

/// A redirected connection that passed the HTTP detection.
pub struct MaybeHttp {
    pub info: ConnectionInfo,
    pub http_version: Option<HttpVersion>,
    pub stream: Box<dyn IncomingIO>,
}

impl MaybeHttp {
    /// Timeout for detemining if the redirected connection is HTTP.
    pub const HTTP_DETECTION_TIMEOUT: Duration = Duration::from_secs(10);

    /// Accepts the TLS connection (optionally) and detects if the redirected connection is
    /// HTTP.
    pub async fn detect(
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

impl fmt::Debug for MaybeHttp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MaybeHttp")
            .field("info", &self.info)
            .field("http_version", &self.http_version)
            .finish()
    }
}

/// [`Stream`] of data from a redirected TCP connection or an HTTP request.
///
/// This stream does not finish before returning the final [`IncomingStreamItem::Finished`] item.
pub struct IncomingStream {
    rx: Option<mpsc::Receiver<IncomingStreamItem>>,
}

impl Stream for IncomingStream {
    type Item = IncomingStreamItem;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        let Some(rx) = this.rx.as_mut() else {
            return Poll::Ready(None);
        };

        let item = match std::task::ready!(rx.poll_recv(cx)) {
            Some(item) => item,
            None => IncomingStreamItem::Finished(Err(ConnError::Dropped)),
        };

        if matches!(item, IncomingStreamItem::Finished(..)) {
            this.rx = None;
        }

        Poll::Ready(Some(item))
    }
}

/// Update from a redirected TCP connection or an HTTP request.
#[derive(Debug)]
pub enum IncomingStreamItem {
    /// Request body frame.
    Frame(InternalHttpBodyFrame),
    /// Request body finished.
    NoMoreFrames,
    /// Data after an HTTP upgrade.
    Data(Vec<u8>),
    /// Data after an HTTP upgrade finished.
    NoMoreData,
    /// Connection/request finished.
    Finished(Result<(), ConnError>),
}
