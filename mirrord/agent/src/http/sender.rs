use hyper::{
    body::{Body, Incoming},
    client::conn::{http1, http2},
    Request, Response,
};

/// [`hyper`] uses different types for HTTP/1 and HTTP/2 request senders.
///
/// This struct is just a simple `Either` type.
pub enum HttpSender<B> {
    V1(http1::SendRequest<B>),
    V2(http2::SendRequest<B>),
}

impl<B: 'static + Body> HttpSender<B> {
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
