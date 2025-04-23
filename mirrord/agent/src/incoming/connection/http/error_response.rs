use std::fmt;

use axum::response::Response;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::http::{StatusCode, Version};

use super::BoxResponse;

/// HTTP response produced by the agent when it fails to serve a redirected request.
///
/// 1. Always uses [`StatusCode::BAD_GATEWAY`].
/// 2. Body always starts with `mirrord-agent: `.
pub struct MirrordErrorResponse {
    version: Version,
    body: Bytes,
}

impl MirrordErrorResponse {
    pub fn new<B: fmt::Display>(version: Version, body: B) -> Self {
        let body = format!("mirrord-agent: {body}\n").into();

        Self { version, body }
    }
}

impl From<MirrordErrorResponse> for BoxResponse {
    fn from(value: MirrordErrorResponse) -> Self {
        Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .version(value.version)
            .body(Full::new(value.body).map_err(|_| unreachable!()).boxed())
            .unwrap()
    }
}
