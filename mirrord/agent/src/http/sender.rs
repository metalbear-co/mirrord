use hyper::{
    Request, Response,
    body::{Body, Incoming},
    client::conn::{http1, http2},
    rt::{Read, Write},
};
use hyper_util::rt::TokioExecutor;

use super::HttpVersion;

/// [`hyper`] uses different types for HTTP/1 and HTTP/2 request senders.
///
/// This struct is just a simple `Either` type.
pub enum HttpSender<B> {
    V1(http1::SendRequest<B>),
    V2(http2::SendRequest<B>),
}

impl<B: 'static + Body> HttpSender<B>
where
    B: 'static + Body + Send + Unpin,
    B::Data: Send,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    pub async fn new<IO>(io: IO, version: HttpVersion) -> hyper::Result<Self>
    where
        IO: 'static + Send + Read + Write + Unpin,
    {
        match version {
            HttpVersion::V1 => {
                let (sender, conn) = http1::handshake(io).await?;
                tokio::spawn(conn.with_upgrades());
                Ok(Self::V1(sender))
            }
            HttpVersion::V2 => {
                let (sender, conn) = http2::handshake(TokioExecutor::default(), io).await?;
                tokio::spawn(conn);
                Ok(Self::V2(sender))
            }
        }
    }

    pub async fn send(&mut self, request: Request<B>) -> hyper::Result<Response<Incoming>> {
        match self {
            Self::V1(sender) => {
                sender.ready().await?;
                sender.send_request(request).await
            }
            Self::V2(sender) => {
                sender.ready().await?;
                sender.send_request(request).await
            }
        }
    }
}
