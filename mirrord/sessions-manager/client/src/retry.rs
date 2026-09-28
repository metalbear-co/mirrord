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

/// One backoff sequence, consumed either by retrying an operation ([`Self::run_until`]) or by
/// waiting out a single step for a caller-driven retry ([`Self::wait_next_delay`]).
///
/// Backoff only advances on failure and only resets when the caller calls [`Self::reset`] —
/// callers reset once they've confirmed the retry actually paid off, on whatever terms make sense
/// for them (e.g. after useful data arrives, not merely after a connection opens).
pub(crate) struct RetryBudget {
    delays: SharedRetryDelays,
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
    pub(crate) fn new() -> Self {
        Self {
            delays: SharedRetryDelays(Arc::new(Mutex::new(init_retry_policy()))),
        }
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

    /// Same as [`Self::run`], but also gives up once `deadline` elapses.
    ///
    /// The deadline can be observed here, or independently inside `operation` itself (which
    /// typically layers its own bounded waits): both report through the same `E`, so callers
    /// don't need to know which layer noticed first.
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
        match with_deadline(deadline, self.run(operation, should_retry)).await {
            Ok(result) => result,
            Err(error) => Err(error.into()),
        }
    }

    /// Draws the next backoff delay and waits it out. For callers that manage their own retry
    /// point (e.g. re-arming local state for an externally-driven retry) rather than retrying one
    /// operation in a loop, so [`Self::run_until`] doesn't fit. Returns the delay drawn so the
    /// caller can log it.
    pub(crate) async fn wait_next_delay(&self) -> Duration {
        let delay = self
            .delays
            .clone()
            .next()
            .expect("exponential backoff strategy is unbounded");
        tokio::time::sleep(delay).await;
        delay
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

/// Runs an operation until it completes or the optional absolute deadline expires. The
/// operation's output is preserved unchanged.
///
/// Nothing else interrupts `future`: a caller that stops caring about the result simply stops
/// polling it, and shutting a spawned task down is the business of whoever spawned it.
pub(crate) async fn with_deadline<F>(
    deadline: Option<Instant>,
    future: F,
) -> Result<F::Output, SessionsManagerClientError>
where
    F: Future,
{
    let timeout = OptionFuture::from(deadline.map(tokio::time::sleep_until));
    tokio::select! {
        Some(()) = timeout => Err(SessionsManagerClientError::OperationTimeout),
        output = future => Ok(output),
    }
}

#[cfg(test)]
mod tests {
    use std::future;

    use tokio::time::Instant;

    use super::with_deadline;
    use crate::error::SessionsManagerClientError;

    #[tokio::test]
    async fn with_deadline_returns_completed_output() {
        let result = with_deadline(None, async { 42 }).await;

        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn with_deadline_returns_timeout_when_deadline_is_expired() {
        let result = with_deadline(Some(Instant::now()), future::pending::<()>()).await;

        assert!(matches!(
            result,
            Err(SessionsManagerClientError::OperationTimeout)
        ));
    }

    #[tokio::test]
    async fn with_deadline_preserves_inner_output() {
        let result = with_deadline(None, async {
            Err::<(), _>(SessionsManagerClientError::SseStreamEnded("test"))
        })
        .await;

        assert!(matches!(
            result,
            Ok(Err(SessionsManagerClientError::SseStreamEnded(_)))
        ));
    }
}
