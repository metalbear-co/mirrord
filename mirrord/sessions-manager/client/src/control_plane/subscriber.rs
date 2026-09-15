use std::sync::Arc;

use tokio::{sync::Mutex, time::Instant};
use tokio_util::sync::CancellationToken;

use crate::{
    control_plane::{ControlPlaneEvent, ControlPlaneEventStream, HttpControlPlaneClient},
    error::SessionsManagerClientError,
    retry::RetryBudget,
};

pub(crate) trait ControlPlaneSubscription {
    type Output;

    fn name(&self) -> &'static str;

    async fn subscribe(
        &self,
        client: &HttpControlPlaneClient,
        cancellation: &CancellationToken,
    ) -> Result<ControlPlaneEventStream, SessionsManagerClientError>;

    fn extract(&self, event: ControlPlaneEvent)
    -> Result<Self::Output, SessionsManagerClientError>;
}

/// Drives one control-plane connection attempt at a time, holding whatever must survive across
/// retries (the not-yet-open-or-reconnecting stream, and metadata like `opened_once`).
struct ControlPlaneSubscriptionDriver<S> {
    client: HttpControlPlaneClient,
    subscription: S,
    events: Option<ControlPlaneEventStream>,
    retry_initialization: bool,
    opened_once: bool,
}

/// Drives a control-plane subscription and yields its typed events across reconnects.
pub(crate) struct ControlPlaneSubscriber<S> {
    state: Arc<Mutex<ControlPlaneSubscriptionDriver<S>>>,
    retry: RetryBudget,
    terminal: bool,
}

impl<S> ControlPlaneSubscriber<S>
where
    S: ControlPlaneSubscription,
{
    pub(crate) fn new(
        client: HttpControlPlaneClient,
        subscription: S,
        cancellation: CancellationToken,
        retry_initialization: bool,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(ControlPlaneSubscriptionDriver {
                client,
                subscription,
                events: None,
                retry_initialization,
                opened_once: false,
            })),
            retry: RetryBudget::new(cancellation),
            terminal: false,
        }
    }

    pub(crate) async fn next(&mut self) -> Option<Result<S::Output, SessionsManagerClientError>> {
        self.next_with_deadline(None).await
    }

    /// Waits for the next event, failing with [`SessionsManagerClientError::OperationTimeout`]
    /// once `deadline` passes.
    ///
    /// Within that budget, a connection that goes silent for [`ControlPlaneEventStream::next`]'s
    /// idle timeout — including SSE keep-alives, not just decoded events — is treated the same as
    /// any other transport error: the subscription reconnects and keeps trying against the
    /// remaining budget, rather than blocking on a connection that's actually dead until
    /// `deadline` itself expires. This makes callers like `crate::client::intproxy`'s bounded
    /// connect flow noticeably more
    /// robust against silently stalled connections, at the cost of a possible reconnect
    /// happening even when `deadline` is nowhere near exhausted.
    pub(crate) async fn next_until(
        &mut self,
        deadline: Instant,
    ) -> Result<S::Output, SessionsManagerClientError> {
        self.next_with_deadline(Some(deadline))
            .await
            .unwrap_or_else(|| Err(SessionsManagerClientError::Cancelled))
    }

    async fn next_with_deadline(
        &mut self,
        deadline: Option<Instant>,
    ) -> Option<Result<S::Output, SessionsManagerClientError>> {
        if self.terminal || self.retry.is_cancelled() {
            return None;
        }

        let event = match self
            .retry
            .run_until(
                deadline,
                || {
                    let cancellation = self.retry.cancellation().clone();
                    let state = self.state.clone();
                    async move {
                        state
                            .lock()
                            .await
                            .next_event_attempt(&cancellation, deadline)
                            .await
                    }
                },
                ControlPlaneAttemptError::should_retry,
            )
            .await
        {
            Ok(event) => event,
            Err(error) => {
                let error = error.into_source();
                if matches!(error, SessionsManagerClientError::Cancelled) {
                    return None;
                }
                self.terminal = true;
                return Some(Err(error));
            }
        };

        let state = self.state.lock().await;
        match state.subscription.extract(event) {
            Ok(output) => {
                self.retry.reset();
                Some(Ok(output))
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    subscription = state.subscription.name(),
                    "sessions-manager control-plane subscription returned a terminal event"
                );
                self.terminal = true;
                Some(Err(error))
            }
        }
    }
}

impl<S> ControlPlaneSubscriptionDriver<S>
where
    S: ControlPlaneSubscription,
{
    async fn next_event_attempt(
        &mut self,
        cancellation: &CancellationToken,
        deadline: Option<Instant>,
    ) -> Result<ControlPlaneEvent, ControlPlaneAttemptError> {
        if self.events.is_none() {
            tracing::debug!(
                subscription = self.subscription.name(),
                ?deadline,
                opened_once = self.opened_once,
                "opening sessions-manager control-plane subscription"
            );
            let retry_initialization = self.opened_once || self.retry_initialization;
            let stream = self
                .subscription
                .subscribe(&self.client, cancellation)
                .await
                .map_err(|error| ControlPlaneAttemptError::Opening {
                    error,
                    retry_initialization,
                })?;

            tracing::debug!(
                subscription = self.subscription.name(),
                "sessions-manager control-plane subscription opened"
            );
            self.events = Some(stream);
            self.opened_once = true;
        }

        let event = match self
            .events
            .as_mut()
            .expect("control-plane subscriber opened an event stream")
            .next()
            .await
        {
            Ok(Some(event)) => event,
            Ok(None) => Err(SessionsManagerClientError::Sse(format!(
                "{} stream ended",
                self.subscription.name()
            ))),
            Err(error) => Err(error),
        };

        // Any failure here — timeout, clean end-of-stream, or a decode error — means this stream
        // is done; drop it so the next retry attempt reopens the subscription instead of reusing
        // one that's known bad.
        event
            .inspect_err(|_| self.events = None)
            .map_err(ControlPlaneAttemptError::Stream)
    }
}

enum ControlPlaneAttemptError {
    Opening {
        error: SessionsManagerClientError,
        retry_initialization: bool,
    },
    Stream(SessionsManagerClientError),
}

impl ControlPlaneAttemptError {
    fn should_retry(&self) -> bool {
        match self {
            Self::Opening {
                error,
                retry_initialization,
            } => *retry_initialization && error.is_retryable(),
            Self::Stream(error) => error.is_retryable(),
        }
    }

    fn into_source(self) -> SessionsManagerClientError {
        match self {
            Self::Opening { error, .. } | Self::Stream(error) => error,
        }
    }
}

impl From<SessionsManagerClientError> for ControlPlaneAttemptError {
    /// Lets [`RetryBudget::run_until`] report a cancellation or deadline it observed itself
    /// through the same error type `operation` uses, so callers see one uniform failure regardless
    /// of which layer noticed it first.
    fn from(error: SessionsManagerClientError) -> Self {
        Self::Stream(error)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Arc, time::Duration};

    use futures::stream;
    use tokio::{
        sync::{Mutex, watch},
        time::Instant,
    };
    use tokio_util::sync::CancellationToken;
    use url::Url;

    use super::{ControlPlaneSubscriber, ControlPlaneSubscription};
    use crate::{
        config::SessionsManagerConfig,
        control_plane::{ControlPlaneEvent, ControlPlaneEventStream, HttpControlPlaneClient},
        credentials::NoCredentials,
        error::SessionsManagerClientError,
    };

    type StreamFactory =
        Box<dyn FnOnce() -> Result<ControlPlaneEventStream, SessionsManagerClientError> + Send>;

    struct TestSubscription {
        streams: Mutex<VecDeque<StreamFactory>>,
    }

    impl TestSubscription {
        fn new(streams: Vec<StreamFactory>) -> Self {
            Self {
                streams: Mutex::new(streams.into()),
            }
        }
    }

    impl ControlPlaneSubscription for TestSubscription {
        type Output = ();

        fn name(&self) -> &'static str {
            "test subscription"
        }

        async fn subscribe(
            &self,
            _client: &HttpControlPlaneClient,
            _cancellation: &CancellationToken,
        ) -> Result<ControlPlaneEventStream, SessionsManagerClientError> {
            // Built lazily, at the moment it's actually handed out, so its `last_activity`
            // baseline matches production (where the watch channel starts ticking exactly when
            // the connection opens) rather than whenever the test assembled its fixtures.
            let factory = self
                .streams
                .lock()
                .await
                .pop_front()
                .expect("test configured another stream result");
            factory()
        }

        fn extract(
            &self,
            event: ControlPlaneEvent,
        ) -> Result<Self::Output, SessionsManagerClientError> {
            match event {
                ControlPlaneEvent::Assignment(_) => Ok(()),
                ControlPlaneEvent::Superseded => Err(SessionsManagerClientError::Superseded),
            }
        }
    }

    fn client() -> HttpControlPlaneClient {
        let config = SessionsManagerConfig {
            environment: "test".to_owned(),
            service: "test".to_owned(),
            base_url: Url::parse("https://sessions.example.com").unwrap(),
        };
        HttpControlPlaneClient::new(&config, Arc::new(NoCredentials)).unwrap()
    }

    fn assignment_event() -> Result<ControlPlaneEvent, SessionsManagerClientError> {
        Ok(ControlPlaneEvent::Assignment(serde_json::from_value(
            serde_json::json!({
                "assignment_id": "assignment-1",
                "data_plane_endpoint": "/ws/test",
                "authorization": "Bearer test",
            }),
        )?))
    }

    fn events(
        events: Vec<Result<ControlPlaneEvent, SessionsManagerClientError>>,
    ) -> ControlPlaneEventStream {
        let (_activity_tx, activity_rx) = watch::channel(Instant::now());
        ControlPlaneEventStream::new(Box::pin(stream::iter(events)), activity_rx)
    }

    fn stalled_events() -> ControlPlaneEventStream {
        let (_activity_tx, activity_rx) = watch::channel(Instant::now());
        ControlPlaneEventStream::new(Box::pin(stream::pending()), activity_rx)
    }

    #[tokio::test]
    async fn retries_initial_open_when_enabled() {
        let subscription = TestSubscription::new(vec![
            Box::new(|| {
                Err(SessionsManagerClientError::Sse(
                    "temporary failure".to_owned(),
                ))
            }),
            Box::new(|| Ok(events(vec![assignment_event()]))),
        ]);
        let mut subscriber =
            ControlPlaneSubscriber::new(client(), subscription, CancellationToken::new(), true);

        assert!(matches!(subscriber.next().await, Some(Ok(()))));
    }

    #[tokio::test]
    async fn reopens_after_a_successful_stream_ends() {
        let subscription = TestSubscription::new(vec![
            Box::new(|| Ok(events(Vec::new()))),
            Box::new(|| Ok(events(vec![assignment_event()]))),
        ]);
        let mut subscriber =
            ControlPlaneSubscriber::new(client(), subscription, CancellationToken::new(), false);

        assert!(
            subscriber
                .next_until(Instant::now() + Duration::from_secs(1))
                .await
                .is_ok()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn reconnects_after_event_read_timeout_without_caller_deadline() {
        let subscription = TestSubscription::new(vec![
            Box::new(|| Ok(stalled_events())),
            Box::new(|| Ok(events(vec![assignment_event()]))),
        ]);
        let mut subscriber =
            ControlPlaneSubscriber::new(client(), subscription, CancellationToken::new(), false);

        // `next()` has no caller deadline, so a stalled connection must be reconnected rather
        // than surfaced as a fatal `OperationTimeout`.
        assert!(matches!(subscriber.next().await, Some(Ok(()))));
    }

    #[tokio::test]
    async fn does_not_retry_initial_open_when_disabled() {
        let subscription = TestSubscription::new(vec![Box::new(|| {
            Err(SessionsManagerClientError::Sse(
                "temporary failure".to_owned(),
            ))
        })]);
        let mut subscriber =
            ControlPlaneSubscriber::new(client(), subscription, CancellationToken::new(), false);

        assert!(matches!(
            subscriber
                .next_until(Instant::now() + Duration::from_secs(1))
                .await,
            Err(SessionsManagerClientError::Sse(_))
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn does_not_reconnect_while_heartbeats_keep_arriving() {
        let (activity_tx, activity_rx) = watch::channel(Instant::now());
        // Only one stream is ever handed out: if a reconnect is attempted, `subscribe()` panics
        // on the empty queue instead of silently masking the regression.
        let subscription = TestSubscription::new(vec![Box::new(move || {
            Ok(ControlPlaneEventStream::new(
                Box::pin(stream::pending()),
                activity_rx,
            ))
        })]);
        let mut subscriber =
            ControlPlaneSubscriber::new(client(), subscription, CancellationToken::new(), false);

        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(Duration::from_secs(15));
            loop {
                ticks.tick().await;
                if activity_tx.send(Instant::now()).is_err() {
                    return;
                }
            }
        });

        // 5 simulated minutes, well past EVENT_READ_TIMEOUT (60s): `next()` must still be
        // pending because heartbeats keep resetting the watchdog.
        let result = tokio::time::timeout(Duration::from_secs(5 * 60), subscriber.next()).await;
        assert!(result.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn next_until_times_out_promptly_on_a_healthy_but_silent_connection() {
        let subscription = TestSubscription::new(vec![Box::new(|| {
            let (_activity_tx, activity_rx) = watch::channel(Instant::now());
            Ok(ControlPlaneEventStream::new(
                Box::pin(stream::pending()),
                activity_rx,
            ))
        })]);
        let mut subscriber =
            ControlPlaneSubscriber::new(client(), subscription, CancellationToken::new(), false);

        // The caller deadline (10s) is well inside EVENT_READ_TIMEOUT (60s), so this must fail
        // with the caller's own deadline rather than waiting out the staleness watchdog.
        let deadline = Instant::now() + Duration::from_secs(10);
        let started = Instant::now();
        let result = subscriber.next_until(deadline).await;
        assert!(matches!(
            result,
            Err(SessionsManagerClientError::OperationTimeout)
        ));
        assert!(started.elapsed() < Duration::from_secs(60));
    }
}
