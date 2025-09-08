#![feature(try_trait_v2)]
#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]
// TODO(alex): Get a big `Box` for the big variants.
#![allow(clippy::large_enum_variant)]

//! # Features
//!
//! ## `incluster`
//!
//! Turn this feature on if you want to connect to agent pods from within the cluster with a plain
//! TCP connection.
//!
//! ## `portforward`
//!
//! Turn this feature on if you want to connect to agent pods from outside the cluster with port
//! forwarding.

use std::{sync::OnceLock, time::Duration};

use http::{Request, StatusCode};
use kube::client::Body;
use mirrord_config::retry::StartupRetryConfig;
use tower::{
    BoxError,
    retry::{
        Policy,
        backoff::{
            Backoff, ExponentialBackoff, ExponentialBackoffMaker, InvalidBackoff, MakeBackoff,
        },
    },
    util::rng::HasherRng,
};
use tracing::Level;

pub mod api;
pub mod error;
pub mod resolved;

pub static RETRY_KUBE_OPERATIONS_POLICY: OnceLock<RetryKube> = OnceLock::new();

use tower_http::{self as _};

#[derive(Clone)]
pub struct RetryKube {
    backoff: ExponentialBackoff,
    max_attempts: u32,
}

impl TryFrom<&StartupRetryConfig> for RetryKube {
    type Error = InvalidBackoff;

    fn try_from(
        StartupRetryConfig {
            min_ms,
            max_ms,
            max_attempts,
        }: &StartupRetryConfig,
    ) -> Result<Self, Self::Error> {
        let backoff = ExponentialBackoffMaker::new(
            Duration::from_millis(*min_ms),
            Duration::from_millis(*max_ms),
            2.0,
            HasherRng::new(),
        )?
        .make_backoff();

        Ok(Self {
            backoff,
            max_attempts: *max_attempts,
        })
    }
}

impl Default for RetryKube {
    fn default() -> Self {
        let retry_config = StartupRetryConfig {
            min_ms: 500,
            max_ms: 5000,
            max_attempts: 2,
        };

        Self::try_from(&retry_config).expect("Default values should be valid!")
    }
}

impl<Res> Policy<http::Request<Body>, http::Response<Res>, BoxError> for RetryKube {
    type Future = tokio::time::Sleep;

    #[tracing::instrument(level = Level::INFO, skip(self, result))]
    fn retry(
        &mut self,
        req: &mut http::Request<Body>,
        result: &mut Result<http::Response<Res>, BoxError>,
    ) -> Option<Self::Future> {
        result.as_ref().ok().and_then(|response| {
            matches!(
                response.status(),
                StatusCode::TOO_MANY_REQUESTS
                    | StatusCode::SERVICE_UNAVAILABLE
                    | StatusCode::GATEWAY_TIMEOUT
            )
            .then(|| self.backoff.next_backoff())
        })
    }

    #[tracing::instrument(level = Level::INFO, skip(self), ret)]
    fn clone_request(&mut self, req: &http::Request<Body>) -> Option<http::Request<Body>> {
        let body = req.body().try_clone()?;

        let mut request = Request::builder()
            .method(req.method().clone())
            .uri(req.uri().clone())
            .version(req.version().clone())
            .extension(req.extensions().clone());

        if let Some(headers) = request.headers_mut() {
            headers.extend(req.headers().clone());
        }

        request.body(body).ok()
    }
}
