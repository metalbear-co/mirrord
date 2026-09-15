use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures::future::OptionFuture;
use tokio::time::Instant;
use tokio_retry::{
    RetryIf,
    strategy::{ExponentialBackoff, jitter},
};
use tokio_util::sync::CancellationToken;

use crate::error::SessionsManagerClientError;

type RetryDelays = Box<dyn Iterator<Item = Duration> + Send>;

fn init_retry_policy() -> RetryDelays {
    Box::new(
        ExponentialBackoff::from_millis(2)
            .factor(50)
            .max_delay(Duration::from_secs(5))
            .map(jitter),
    )
}

/// One backoff sequence bounded by one owned cancellation token, consumed either by retrying an
/// operation ([`Self::run_until`]) or by waiting out a single step for a caller-driven retry
/// ([`Self::wait_next_delay`]).
///
/// Owning `cancellation` lets every retry call site check liveness and bound retries through this
/// one type, instead of separately tracking and re-passing a `&CancellationToken` alongside it.
/// Backoff only advances on failure and only resets when the caller calls [`Self::reset`] —
/// callers reset once they've confirmed the retry actually paid off, on whatever terms make sense
/// for them (e.g. after useful data arrives, not merely after a connection opens).
#[derive(Clone)]
pub(crate) struct RetryBudget {
    delays: SharedRetryDelays,
    cancellation: CancellationToken,
}

/// Shares one retry-delay sequence between short-lived [`RetryIf`] operations.
#[derive(Clone)]
struct SharedRetryDelays(Arc<Mutex<RetryDelays>>);

impl Iterator for SharedRetryDelays {
    type Item = Duration;

    fn next(&mut self) -> Option<Self::Item> {
        self.0
            .lock()
            .expect("retry budget state mutex poisoned")
            .next()
    }
}

impl RetryBudget {
    pub(crate) fn new(cancellation: CancellationToken) -> Self {
        Self {
            delays: SharedRetryDelays(Arc::new(Mutex::new(init_retry_policy()))),
            cancellation,
        }
    }

    pub(crate) fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    /// Retries an operation while `should_retry` accepts its error.
    async fn run<T, E, F, Fut, C>(&self, operation: F, should_retry: C) -> Result<T, E>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, E>>,
        C: FnMut(&E) -> bool,
    {
        RetryIf::start(self.delays.clone(), operation, should_retry).await
    }

    /// Same as [`Self::run`], but also gives up once this retry's cancellation token fires or
    /// `deadline` elapses.
    ///
    /// A cancellation or deadline can be observed here, or independently inside `operation`
    /// itself (which typically layers its own interruptible waits): both report through the same
    /// `E`, so callers don't need to know which layer noticed first.
    pub(crate) async fn run_until<T, E, F, Fut, C>(
        &self,
        deadline: Option<Instant>,
        operation: F,
        should_retry: C,
    ) -> Result<T, E>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, E>>,
        C: FnMut(&E) -> bool,
        E: From<SessionsManagerClientError>,
    {
        match run_interruptible(
            &self.cancellation,
            deadline,
            self.run(operation, should_retry),
        )
        .await
        {
            Ok(result) => result,
            Err(error) => Err(error.into()),
        }
    }

    /// Draws the next backoff delay and waits it out. For callers that manage their own retry
    /// point (e.g. re-arming local state for an externally-driven retry) rather than retrying one
    /// operation in a loop, so [`Self::run_until`] doesn't fit. Returns the delay drawn so the
    /// caller can log it.
    pub(crate) async fn wait_next_delay(
        &self,
        deadline: Option<Instant>,
    ) -> Result<Duration, SessionsManagerClientError> {
        let delay = self
            .delays
            .clone()
            .next()
            .expect("exponential backoff strategy is unbounded");
        wait_retry(&self.cancellation, deadline, delay).await?;
        Ok(delay)
    }

    /// Resets the backoff after receiving useful work from the connection.
    pub(crate) fn reset(&self) {
        *self
            .delays
            .0
            .lock()
            .expect("retry budget state mutex poisoned") = init_retry_policy();
    }
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
