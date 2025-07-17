use std::{
    fmt, io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use actix_codec::ReadBuf;
use bytes::Bytes;
use futures::Stream;
use mirrord_protocol::tcp::InternalHttpBodyFrame;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc,
};
use tokio_stream::wrappers::{errors::BroadcastStreamRecvError, BroadcastStream};

use super::{
    error::{ConnError, HttpDetectError},
    tls::{self, handler::PassThroughTlsConnector, StealTlsHandlerStore},
    Redirected,
};
use crate::{
    http::HttpVersion,
    metrics::{MetricGuard, REDIRECTED_CONNECTIONS},
};

mod copy_bidirectional;
pub mod http;
mod http_task;
mod optional_broadcast;
pub mod tcp;

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
/// Having a super trait allows us to return [`Box<dyn IncomingIO>`]
/// and simplify the code in general.
pub trait IncomingIO: 'static + AsyncRead + AsyncWrite + Send + Sync + Unpin {}

impl<T> IncomingIO for T where T: 'static + AsyncRead + AsyncWrite + Send + Sync + Unpin {}

/// Wrapper over an incoming IO stream.
///
/// Automatically updates the [`REDIRECTED_CONNECTIONS`] metric with an internal [`MetricGuard`].
/// Transparently implements [`AsyncRead`] and [`AsyncWrite`].
struct IncomingIoWrapper<T> {
    io: T,
    _metric_guard: MetricGuard,
}

impl<T> AsyncRead for IncomingIoWrapper<T>
where
    T: AsyncRead + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        Pin::new(&mut this.io).poll_read(cx, buf)
    }
}

impl<T> AsyncWrite for IncomingIoWrapper<T>
where
    T: AsyncWrite + Unpin,
{
    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        let this = self.get_mut();
        Pin::new(&mut this.io).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        let this = self.get_mut();
        Pin::new(&mut this.io).poll_shutdown(cx)
    }

    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let this = self.get_mut();
        Pin::new(&mut this.io).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<Result<usize, io::Error>> {
        let this = self.get_mut();
        Pin::new(&mut this.io).poll_write_vectored(cx, bufs)
    }
}

/// A redirected connection that passed the HTTP detection.
///
/// # Metrics
///
/// This type handles managing the [`REDIRECTED_CONNECTIONS`] metric,
/// you don't need to do it manually.
///
/// The metric is incremented in [`MaybeHttp::detect`] and decremented on [`MaybeHttp::stream`]
/// drop.
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
        let metric_guard = MetricGuard::new(&REDIRECTED_CONNECTIONS);

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
                stream: Box::new(IncomingIoWrapper {
                    io: stream,
                    _metric_guard: metric_guard,
                }),
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
            Some(tls::HTTP_2_ALPN_NAME) => (
                Box::new(IncomingIoWrapper {
                    io: stream,
                    _metric_guard: metric_guard,
                }),
                Some(HttpVersion::V2),
            ),
            Some(tls::HTTP_1_1_ALPN_NAME | tls::HTTP_1_0_ALPN_NAME) => (
                Box::new(IncomingIoWrapper {
                    io: stream,
                    _metric_guard: metric_guard,
                }),
                Some(HttpVersion::V1),
            ),
            Some(..) => (
                Box::new(IncomingIoWrapper {
                    io: stream,
                    _metric_guard: metric_guard,
                }),
                None,
            ),
            None => {
                let (stream, http_version) =
                    crate::http::detect_http_version(stream, Self::HTTP_DETECTION_TIMEOUT)
                        .await
                        .map_err(HttpDetectError::HttpDetect)?;
                (
                    Box::new(IncomingIoWrapper {
                        io: stream,
                        _metric_guard: metric_guard,
                    }),
                    http_version,
                )
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
pub enum IncomingStream {
    Steal(mpsc::Receiver<IncomingStreamItem>),
    Mirror(
        /// [`tokio::sync::broadcast::Receiver`] has no `poll` method,
        /// we need to use a wrapper.
        BroadcastStream<IncomingStreamItem>,
    ),
    Exhausted,
}

impl Stream for IncomingStream {
    type Item = IncomingStreamItem;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        let item = match this {
            Self::Steal(rx) => std::task::ready!(rx.poll_recv(cx)).unwrap_or(
                IncomingStreamItem::Finished(Err(ConnError::AgentBug(format!(
                    "connection task dropped the channel before sending the Finished item [{}:{}]",
                    file!(),
                    line!(),
                )))),
            ),
            Self::Mirror(rx) => match std::task::ready!(Pin::new(rx).poll_next(cx)) {
                Some(Ok(item)) => item,
                Some(Err(BroadcastStreamRecvError::Lagged(..))) => {
                    IncomingStreamItem::Finished(Err(ConnError::BroadcastLag))
                }
                None => IncomingStreamItem::Finished(Err(ConnError::AgentBug(format!(
                    "connection task dropped the channel before sending the Finished item [{}:{}]",
                    file!(),
                    line!(),
                )))),
            },
            Self::Exhausted => return Poll::Ready(None),
        };

        if matches!(item, IncomingStreamItem::Finished(..)) {
            *this = Self::Exhausted;
        }

        Poll::Ready(Some(item))
    }
}

impl fmt::Debug for IncomingStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            IncomingStream::Steal(_) => "Steal",
            IncomingStream::Mirror(_) => "Mirror",
            IncomingStream::Exhausted => "Exhausted",
        };
        f.write_str(name)
    }
}

/// Update from a redirected TCP connection or an HTTP request.
#[derive(Debug, Clone)]
pub enum IncomingStreamItem {
    /// Request body frame.
    Frame(InternalHttpBodyFrame),
    /// Request body finished.
    NoMoreFrames,
    /// Data after an HTTP upgrade.
    Data(Bytes),
    /// No more data after an HTTP upgrade.
    NoMoreData,
    /// Connection/request finished.
    Finished(Result<(), ConnError>),
}
