use std::fmt;

use axum::response::Response;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::http::{StatusCode, Version};

use super::BoxResponse;

/// HTTP response produced by the agent when it fails to serve a redirected request.
///
/// The body always starts with `mirrord-agent v<version>: `.
pub struct MirrordErrorResponse {
    version: Version,
    status: StatusCode,
    body: Bytes,
}

impl MirrordErrorResponse {
    /// Generic failure response. Uses [`StatusCode::BAD_GATEWAY`].
    pub fn new<M: fmt::Display>(version: Version, message: M) -> Self {
        Self::with_status(version, StatusCode::BAD_GATEWAY, message)
    }

    /// Response for a request that was bypassed (did not match any steal filter)
    /// but had no local application listening to serve it.
    ///
    /// This is the expected outcome on a copy-target pod, where the target container
    /// is a placeholder with no real application: only requests matching the steal
    /// filter are served by the stealing client, and everything else is passed
    /// through to `localhost`, where nothing is listening.
    pub fn bypassed(version: Version, port: u16) -> Self {
        Self::with_status(
            version,
            StatusCode::SERVICE_UNAVAILABLE,
            format_args!(
                "request to port {port} did not match the steal filter, and no \
                 application is listening locally to serve it. Use a steal-all \
                 session, or expect filtered-out requests to fail."
            ),
        )
    }

    fn with_status<M: fmt::Display>(version: Version, status: StatusCode, message: M) -> Self {
        let body = format!("mirrord-agent v{}: {message}\n", env!("CARGO_PKG_VERSION")).into();

        Self {
            version,
            status,
            body,
        }
    }
}

impl From<MirrordErrorResponse> for BoxResponse {
    fn from(value: MirrordErrorResponse) -> Self {
        Response::builder()
            .status(value.status)
            .version(value.version)
            .body(Full::new(value.body).map_err(|_| unreachable!()).boxed())
            .unwrap()
    }
}
