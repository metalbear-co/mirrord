use std::convert::Infallible;

use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Full, StreamBody};
use hyper::{
    body::{Body, Frame},
    Response,
};
use mirrord_protocol::{
    tcp::{HttpResponse, InternalHttpBody},
    ConnectionId, RequestId,
};
use tokio_stream::wrappers::ReceiverStream;

pub type ReceiverStreamBody = StreamBody<ReceiverStream<Result<Frame<Bytes>, Infallible>>>;

#[derive(Debug)]
pub enum HttpResponseFallback {
    Framed(HttpResponse<InternalHttpBody>),
    Fallback(HttpResponse<Vec<u8>>),
    Streamed(HttpResponse<ReceiverStreamBody>),
}

impl HttpResponseFallback {
    /// Checks `http_body::Body::is_end_stream`, returning `true` if this response is done.
    ///
    /// Used by our metrics system to decrement the `HTTP_REQUEST_IN_PROGRESS_COUNT` counter
    /// when an streamed HTTP request has finished.
    pub fn is_last(&self) -> bool {
        match self {
            HttpResponseFallback::Framed(http_response) => {
                http_response.internal_response.body.is_end_stream()
            }
            HttpResponseFallback::Fallback(_) => true,
            HttpResponseFallback::Streamed(http_response) => {
                http_response.internal_response.body.is_end_stream()
            }
        }
    }

    pub fn connection_id(&self) -> ConnectionId {
        match self {
            HttpResponseFallback::Framed(req) => req.connection_id,
            HttpResponseFallback::Fallback(req) => req.connection_id,
            HttpResponseFallback::Streamed(req) => req.connection_id,
        }
    }

    pub fn request_id(&self) -> RequestId {
        match self {
            HttpResponseFallback::Framed(req) => req.request_id,
            HttpResponseFallback::Fallback(req) => req.request_id,
            HttpResponseFallback::Streamed(req) => req.request_id,
        }
    }

    pub fn into_hyper<E>(self) -> Response<BoxBody<Bytes, E>> {
        match self {
            HttpResponseFallback::Framed(req) => req
                .internal_response
                .map_body(|body| body.map_err(|_| unreachable!()).boxed())
                .into(),
            HttpResponseFallback::Fallback(req) => req
                .internal_response
                .map_body(|body| {
                    Full::new(Bytes::from_owner(body))
                        .map_err(|_| unreachable!())
                        .boxed()
                })
                .into(),
            HttpResponseFallback::Streamed(req) => req
                .internal_response
                .map_body(|body| body.map_err(|_| unreachable!()).boxed())
                .into(),
        }
    }
}
