use std::fmt;

use axum::response::Response;
use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::http::{StatusCode, Version};

/// HTTP response produced by the agent when it fails to serve a redirected request.
///
/// 1. Always uses [`StatusCode::BAD_GATEWAY`].
/// 2. Body always starts with `mirrord-agent: `.
pub struct MirrordErrorResponse {
    version: Version,
    body: Bytes,
}

impl MirrordErrorResponse {
    pub fn new<M: fmt::Display>(version: Version, message: M) -> Self {
        let body = format!("mirrord-agent: {message}\n").into();

        Self { version, body }
    }
}

impl From<MirrordErrorResponse> for Response<BoxBody<Bytes, hyper::Error>> {
    fn from(value: MirrordErrorResponse) -> Self {
        Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .version(value.version)
            .body(Full::new(value.body).map_err(|_| unreachable!()).boxed())
            .unwrap()
    }
}
