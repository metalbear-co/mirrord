use std::{future::Future, time::Duration};

use futures::future::OptionFuture;
use tokio::time::Instant;
use tokio_retry::strategy::{ExponentialBackoff, jitter};
use tokio_util::sync::CancellationToken;

use crate::error::SessionsManagerClientError;

pub(crate) type RetryDelays = Box<dyn Iterator<Item = Duration> + Send>;

pub(crate) fn init_retry_policy() -> RetryDelays {
    Box::new(
        ExponentialBackoff::from_millis(2)
            .factor(50)
            .max_delay(Duration::from_secs(5))
            .map(jitter),
    )
}

/// Runs an operation until it completes, cancellation is requested, or the optional absolute
/// deadline expires. The operation's output is preserved unchanged.
pub(crate) async fn run_interruptible<F>(
    cancellation: &CancellationToken,
    deadline: Option<Instant>,
    future: F,
) -> Result<F::Output, SessionsManagerClientError>
where
    F: Future,
{
    let timeout = OptionFuture::from(deadline.map(tokio::time::sleep_until));
    tokio::select! {
        _ = cancellation.cancelled() => Err(SessionsManagerClientError::Cancelled),
        Some(()) = timeout => Err(SessionsManagerClientError::OperationTimeout),
        output = future => Ok(output),
    }
}

/// Waits for a retry delay without extending the operation's absolute deadline.
pub(crate) async fn wait_retry(
    cancellation: &CancellationToken,
    deadline: Option<Instant>,
    delay: Duration,
) -> Result<(), SessionsManagerClientError> {
    run_interruptible(cancellation, deadline, tokio::time::sleep(delay)).await
}

/// Draws the next delay from an unbounded backoff iterator and waits for it, returning the delay
/// drawn so the caller can log it with whatever context (an error, an assignment id, ...) is
/// relevant at that retry site. Panics if `retry_delays` is exhausted, which never happens for
/// [`init_retry_policy`]'s policy.
pub(crate) async fn wait_next_retry_delay(
    retry_delays: &mut RetryDelays,
    cancellation: &CancellationToken,
    deadline: Option<Instant>,
) -> Result<Duration, SessionsManagerClientError> {
    let delay = retry_delays
        .next()
        .expect("exponential backoff strategy is unbounded");
    wait_retry(cancellation, deadline, delay).await?;
    Ok(delay)
}

#[cfg(test)]
mod tests {
    use std::{future, time::Duration};

    use tokio::time::Instant;
    use tokio_util::sync::CancellationToken;

    use super::{run_interruptible, wait_retry};
    use crate::error::SessionsManagerClientError;

    #[tokio::test]
    async fn run_interruptible_returns_completed_output() {
        let result = run_interruptible(&CancellationToken::new(), None, async { 42 }).await;

        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn run_interruptible_returns_cancelled_when_cancelled() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let result = run_interruptible(&cancellation, None, future::pending::<()>()).await;

        assert!(matches!(result, Err(SessionsManagerClientError::Cancelled)));
    }

    #[tokio::test]
    async fn run_interruptible_returns_timeout_when_deadline_is_expired() {
        let cancellation = CancellationToken::new();
        let result =
            run_interruptible(&cancellation, Some(Instant::now()), future::pending::<()>()).await;

        assert!(matches!(
            result,
            Err(SessionsManagerClientError::OperationTimeout)
        ));
    }

    #[tokio::test]
    async fn run_interruptible_preserves_inner_output() {
        let result = run_interruptible(&CancellationToken::new(), None, async {
            Err::<(), _>(SessionsManagerClientError::Sse("test".to_owned()))
        })
        .await;

        assert!(matches!(
            result,
            Ok(Err(SessionsManagerClientError::Sse(_)))
        ));
    }

    #[tokio::test]
    async fn wait_retry_completes_zero_duration_delay() {
        let result = wait_retry(&CancellationToken::new(), None, Duration::ZERO).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn wait_retry_returns_cancelled_when_cancelled() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let result = wait_retry(&cancellation, None, Duration::from_secs(1)).await;

        assert!(matches!(result, Err(SessionsManagerClientError::Cancelled)));
    }

    #[tokio::test]
    async fn wait_retry_returns_timeout_when_deadline_is_expired() {
        let cancellation = CancellationToken::new();
        let result = wait_retry(&cancellation, Some(Instant::now()), Duration::from_secs(1)).await;

        assert!(matches!(
            result,
            Err(SessionsManagerClientError::OperationTimeout)
        ));
    }
}
