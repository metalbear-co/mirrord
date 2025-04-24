use std::future::Future;

use bytes::Bytes;
use futures::future::Either;
use http_body_util::combinators::BoxBody;
use hyper::{
    body::Incoming,
    server::conn::{http1, http2},
    service::Service,
    Error, Request, Response,
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::io::{AsyncRead, AsyncWrite};

use super::HttpVersion;

/// Runs hyper's HTTP server on the given IO stream.
///
/// `shutdown` [`Future`] can be used to gracefully shutdown the HTTP connection.
pub async fn run_http_server<IO, F, S>(
    stream: IO,
    service: S,
    version: HttpVersion,
    shutdown: F,
) -> hyper::Result<()>
where
    IO: 'static + AsyncRead + AsyncWrite + Unpin + Send,
    S: 'static
        + Service<Request<Incoming>, Response = Response<BoxBody<Bytes, Error>>, Error = Error>
        + Send
        + Sync,
    S::Future: Send,
    F: Future,
{
    let mut connection = match version {
        HttpVersion::V1 => {
            let conn = http1::Builder::new()
                .preserve_header_case(true)
                .serve_connection(TokioIo::new(stream), service)
                .with_upgrades();
            Either::Left(conn)
        }

        HttpVersion::V2 => {
            let conn = http2::Builder::new(TokioExecutor::default())
                .serve_connection(TokioIo::new(stream), service);
            Either::Right(conn)
        }
    };

    tokio::select! {
        result = &mut connection => result,
        _ = shutdown => {
            match &mut connection {
                Either::Left(conn) => {
                    Pin::new(conn).graceful_shutdown();
                }
                Either::Right(conn) => {
                    Pin::new(conn).graceful_shutdown();
                }
            }

            connection.await
        }
    }
}
