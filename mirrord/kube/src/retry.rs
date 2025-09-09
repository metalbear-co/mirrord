use std::time::Duration;

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

/// Implements [`tower`] [`Policy`] for retrying kube operations for [`kube::Client`].
///
/// You can add it as a [`tower::Layer`] like this:
///
/// ```ignore
/// client_builder
///   .with_layer(&BufferLayer::new(1024))
///   .with_layer(&RetryLayer::new(RetryKube::try_from(&config.startup_retry)?))
/// ```
///
/// You should build this using the [`TryFrom<&StartupRetryConfig>`] impl.
///
/// - We need the `BufferLayer` so the service implements `Clone`.
///
/// - We're assuming that [`Request`] coming from a [`kube::Client`] use the [`Body`] with
///   `Kind::Once(Option<Bytes>)`, since we need to clone the [`Request`] in
///   [`Policy::clone_request`], and the other [`Body`] type cannot be cloned
///   (`Kind::Wrap(UnsyncBoxBody<Bytes, Box<dyn StdError + Send + Sync>>)`). If [`kube`] ever
///   changes this, then we need to change how we retry this stuff.
#[derive(Clone)]
pub struct RetryKube {
    /// We call [`ExponentialBackoff::next_backoff`] to get the sleep time for the next retry.
    backoff: ExponentialBackoff,

    /// Keeps track of how many retries, so we can stop when we reach `max_attempts`.
    current_attempt: u32,

    /// Limits the amount of retries.
    ///
    /// If this is set to `0`, then we don't retry (the request is made once, but if it fails we
    /// don't retry it).
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
            current_attempt: 0,
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

    #[tracing::instrument(level = Level::TRACE, skip(self, result))]
    fn retry(
        &mut self,
        req: &mut http::Request<Body>,
        result: &mut Result<http::Response<Res>, BoxError>,
    ) -> Option<Self::Future> {
        match result {
            Ok(response)
                if matches!(
                    response.status(),
                    StatusCode::TOO_MANY_REQUESTS
                        | StatusCode::SERVICE_UNAVAILABLE
                        | StatusCode::GATEWAY_TIMEOUT
                ) && self.current_attempt < self.max_attempts =>
            {
                self.current_attempt += 1;
                Some(self.backoff.next_backoff())
            }
            _ => None,
        }
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), ret)]
    fn clone_request(&mut self, req: &http::Request<Body>) -> Option<http::Request<Body>> {
        let body = req.body().try_clone()?;

        let mut request = Request::builder()
            .method(req.method().clone())
            .uri(req.uri().clone())
            .version(req.version())
            .extension(req.extensions().clone());

        if let Some(headers) = request.headers_mut() {
            headers.extend(req.headers().clone());
        }

        request.body(body).ok()
    }
}
