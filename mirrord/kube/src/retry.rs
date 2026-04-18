//! Implementation of a helper to convert between a [`StartupRetryConfig`] to the [`kube`] type
//! [`RetryPolicy`], that can be used as a [`tower`] layer to retry `kube` requests that fail with
//! recoverable errors.
use std::time::Duration;

use kube::client::retry::RetryPolicy;
use mirrord_config::retry::StartupRetryConfig;
use tower::retry::backoff::InvalidBackoff;

/// Creates a [`RetryPolicy`] for a [`kube::Client`] from a [`StartupRetryConfig`].
///
/// We use this `RetryPolicy` to retry some `kube` requests during mirrord startup. See
/// [`RetryPolicy`] docs for the errors that are retryable.
pub fn retry_policy_from_config(
    StartupRetryConfig {
        min_ms,
        max_ms,
        max_retries,
    }: &StartupRetryConfig,
) -> Result<RetryPolicy, InvalidBackoff> {
    RetryPolicy::new(
        Duration::from_millis(*min_ms),
        Duration::from_millis(*max_ms),
        *max_retries,
    )
}
