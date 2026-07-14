//! Reconnecting subscriptions to the control-plane SSE feeds.
//!
//! A control-plane attachment is long-lived and expected to break — the transport drops, the
//! server restarts, the connection silently stalls — so callers get a plain "next event" API that
//! hides reconnects, but still fails fast where reconnecting would be wrong. Three pieces:
//!
//! - [`ControlPlaneSubscription`]: the trait describing one feed — what to open, how to read what
//!   comes back. The only feed-specific part; holds no connection.
//! - Implementations live with their feed, not here. `AssignmentSubscription` in
//!   [`crate::assignments`] is the only one.
//! - [`ControlPlaneSubscriber`]: the caller-facing type, generic over that trait. Yields events one
//!   at a time, retrying underneath.
//! - [`ControlPlaneSubscriptionDriver`]: the subscriber's private inner state — the open stream and
//!   per-attempt bookkeeping, split out because it must outlive any single attempt.

use std::sync::Arc;

use tokio::{sync::Mutex, time::Instant};

use crate::{
    control_plane::{ControlPlaneEvent, ControlPlaneEventStream, HttpControlPlaneClient},
    error::SessionsManagerClientError,
    retry::RetryBudget,
};

/// A single control-plane feed: what to subscribe to, and how to interpret its events.
///
/// Implementors carry only the subscription's identity — the parameters sent to the server. The
/// connection, its retries and all cross-attempt state live in [`ControlPlaneSubscriptionDriver`]
/// and [`ControlPlaneSubscriber`], so an implementor must tolerate being re-subscribed any number
/// of times.
pub(crate) trait ControlPlaneSubscription {
    type Output;

    /// Static label identifying this feed in logs and errors.
    fn name(&self) -> &'static str;

    /// Opens the event stream.
    ///
    /// Called once per connection attempt, reconnects included, so it must be safe to re-issue
    /// against a registration the server already knows about.
    async fn subscribe(
        &self,
        client: &HttpControlPlaneClient,
    ) -> Result<ControlPlaneEventStream, SessionsManagerClientError>;

    /// Maps a wire event to the caller-facing output.
    ///
    /// An error here marks the event terminal: [`ControlPlaneSubscriber`] ends the subscription
    /// instead of reconnecting. [`ControlPlaneEvent::Superseded`] is the motivating case — a newer
    /// attachment has taken over this identity, and reopening would make the two clients displace
    /// each other indefinitely.
    fn extract(&self, event: ControlPlaneEvent)
    -> Result<Self::Output, SessionsManagerClientError>;
}

/// Drives a control-plane subscription and yields its typed events across reconnects.
///
/// Transport failures are absorbed: a dropped, ended or stalled stream is reopened under
/// [`RetryBudget`]'s backoff without the caller seeing it. The subscription ends only when the
/// retry budget is exhausted, or when [`ControlPlaneSubscription::extract`] reports a terminal
/// event.
///
/// Nothing here needs to be shut down: it drives no task of its own, so a caller that no longer
/// wants an event just stops polling and drops the subscriber.
pub(crate) struct ControlPlaneSubscriber<S> {
    state: Arc<Mutex<ControlPlaneSubscriptionDriver<S>>>,
    retry: RetryBudget,
    /// Latches once the subscription is over for good, so later calls report that instead of
    /// reopening a stream the server or the caller is already done with.
    closed: bool,
}

impl<S> ControlPlaneSubscriber<S>
where
    S: ControlPlaneSubscription,
{
    /// `retry_initial_open` decides whether a failure to open the *first* stream is retried;
    /// reconnects after a successful open are always retried when the error allows it.
    pub(crate) fn new(
        client: HttpControlPlaneClient,
        subscription: S,
        retry_initial_open: bool,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(ControlPlaneSubscriptionDriver {
                client,
                subscription,
                stream: None,
                retry_initial_open,
                opened_once: false,
            })),
            retry: RetryBudget::new(),
            closed: false,
        }
    }

    /// Waits for the next event, failing with [`SessionsManagerClientError::OperationTimeout`]
    /// when `deadline` passes.
    ///
    /// A connection that goes silent past [`ControlPlaneEventStream::next`]'s idle timeout is
    /// treated as a transport error and reopened within the remaining deadline. This includes SSE
    /// keep-alives, which maintain stream liveness without producing decoded events.
    pub(crate) async fn next(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<S::Output, SessionsManagerClientError> {
        if self.closed {
            return Err(SessionsManagerClientError::SubscriptionClosed);
        }

        let event = match self
            .retry
            .run_until(
                deadline,
                || {
                    let state = self.state.clone();
                    async move { state.lock().await.next_event_attempt(deadline).await }
                },
                ControlPlaneAttemptError::should_retry,
            )
            .await
        {
            Ok(event) => event,
            Err(error) => {
                self.closed = true;
                return Err(error.into_source());
            }
        };

        let state = self.state.lock().await;
        match state.subscription.extract(event) {
            Ok(output) => {
                self.retry.reset();
                Ok(output)
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    subscription = state.subscription.name(),
                    "sessions-manager control-plane subscription returned a terminal event"
                );
                self.closed = true;
                Err(error)
            }
        }
    }
}

/// Drives one control-plane connection attempt at a time, holding whatever must survive across
/// retries (the not-yet-open-or-reconnecting stream, and metadata like `opened_once`).
///
/// Kept apart from [`ControlPlaneSubscriber`] because [`RetryBudget::run_until`] may invoke its
/// operation many times, so anything an attempt mutates has to live outside the closure it calls.
struct ControlPlaneSubscriptionDriver<S> {
    client: HttpControlPlaneClient,
    subscription: S,
    /// The open stream, or [`None`] when the next attempt has to (re)open one.
    stream: Option<ControlPlaneEventStream>,
    /// Whether failing to open the very first stream is worth retrying.
    ///
    /// Long-lived subscribers set this, as nobody is waiting on the first event. Bounded connect
    /// flows leave it unset, so an unreachable control plane or a rejected request surfaces
    /// immediately instead of after a full retry budget.
    retry_initial_open: bool,
    /// Whether a stream has ever opened successfully.
    ///
    /// Once one has, every later open is a reconnect rather than initialization, and is retried
    /// regardless of `retry_initial_open`.
    opened_once: bool,
}

impl<S> ControlPlaneSubscriptionDriver<S>
where
    S: ControlPlaneSubscription,
{
    /// Runs one attempt: opens the stream if it isn't open, then waits for a single event.
    ///
    /// Failures are classified into [`ControlPlaneAttemptError`] so the caller's retry predicate
    /// can tell an attempt that never got off the ground apart from an established stream that
    /// broke.
    async fn next_event_attempt(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<ControlPlaneEvent, ControlPlaneAttemptError> {
        if self.stream.is_none() {
            tracing::debug!(
                subscription = self.subscription.name(),
                ?deadline,
                opened_once = self.opened_once,
                "opening sessions-manager control-plane subscription"
            );
            let retry_initial_open = self.opened_once || self.retry_initial_open;
            let stream = self
                .subscription
                .subscribe(&self.client)
                .await
                .map_err(|error| ControlPlaneAttemptError::Opening {
                    error,
                    retry_initial_open,
                })?;

            tracing::debug!(
                subscription = self.subscription.name(),
                "sessions-manager control-plane subscription opened"
            );
            self.stream = Some(stream);
            self.opened_once = true;
        }

        let event = match self
            .stream
            .as_mut()
            .expect("control-plane subscriber opened an event stream")
            .next()
            .await
        {
            Ok(Some(event)) => event,
            Ok(None) => Err(SessionsManagerClientError::SseStreamEnded(
                self.subscription.name(),
            )),
            Err(error) => Err(error),
        };

        // Any failure here — timeout, clean end-of-stream, or a decode error — means this stream
        // is done; drop it so the next retry attempt reopens the subscription instead of reusing
        // one that's known bad.
        event
            .inspect_err(|_| self.stream = None)
            .map_err(ControlPlaneAttemptError::Stream)
    }
}

/// Why one [`ControlPlaneSubscriptionDriver::next_event_attempt`] failed.
///
/// The distinction only matters for retryability: opening carries the extra
/// initialization-versus-reconnect gate, while a broken established stream is retryable whenever
/// the underlying error is.
enum ControlPlaneAttemptError {
    /// The stream could not be opened.
    Opening {
        error: SessionsManagerClientError,
        /// Resolved at the attempt site: set when this open is a reconnect, or when the subscriber
        /// was configured to retry its initial open.
        retry_initial_open: bool,
    },
    /// An established stream failed, ended, or produced an event that could not be decoded.
    Stream(SessionsManagerClientError),
}

impl ControlPlaneAttemptError {
    fn should_retry(&self) -> bool {
        match self {
            Self::Opening {
                error,
                retry_initial_open,
            } => *retry_initial_open && error.is_retryable(),
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
    /// Lets [`RetryBudget::run_until`] report a deadline it observed itself through the same error
    /// type `operation` uses, so callers see one uniform failure regardless of which layer noticed
    /// it first.
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
    use url::Url;

    use super::{ControlPlaneSubscriber, ControlPlaneSubscription};
    use crate::{
        ServiceScope,
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
            scope: ServiceScope {
                environment: "test".to_owned(),
                service: "test".to_owned(),
            },
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
            Box::new(|| Err(SessionsManagerClientError::SseStreamEnded("temporary"))),
            Box::new(|| Ok(events(vec![assignment_event()]))),
        ]);
        let mut subscriber = ControlPlaneSubscriber::new(client(), subscription, true);

        assert!(matches!(subscriber.next(None).await, Ok(())));
    }

    #[tokio::test]
    async fn reopens_after_a_successful_stream_ends() {
        let subscription = TestSubscription::new(vec![
            Box::new(|| Ok(events(Vec::new()))),
            Box::new(|| Ok(events(vec![assignment_event()]))),
        ]);
        let mut subscriber = ControlPlaneSubscriber::new(client(), subscription, false);

        assert!(
            subscriber
                .next(Some(Instant::now() + Duration::from_secs(1)))
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
        let mut subscriber = ControlPlaneSubscriber::new(client(), subscription, false);

        // Without a caller deadline, a stalled connection must be reconnected rather than
        // surfaced as a fatal `OperationTimeout`.
        assert!(matches!(subscriber.next(None).await, Ok(())));
    }

    #[tokio::test]
    async fn does_not_retry_initial_open_when_disabled() {
        let subscription = TestSubscription::new(vec![Box::new(|| {
            Err(SessionsManagerClientError::SseStreamEnded("temporary"))
        })]);
        let mut subscriber = ControlPlaneSubscriber::new(client(), subscription, false);

        assert!(matches!(
            subscriber
                .next(Some(Instant::now() + Duration::from_secs(1)))
                .await,
            Err(SessionsManagerClientError::SseStreamEnded(_))
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
        let mut subscriber = ControlPlaneSubscriber::new(client(), subscription, false);

        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(Duration::from_secs(15));
            loop {
                ticks.tick().await;
                if activity_tx.send(Instant::now()).is_err() {
                    return;
                }
            }
        });

        // 5 simulated minutes, well past EVENT_READ_TIMEOUT (60s): the call must still be
        // pending because heartbeats keep resetting the watchdog.
        let result = tokio::time::timeout(Duration::from_secs(5 * 60), subscriber.next(None)).await;
        assert!(result.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn next_times_out_promptly_on_a_healthy_but_silent_connection() {
        let subscription = TestSubscription::new(vec![Box::new(|| {
            let (_activity_tx, activity_rx) = watch::channel(Instant::now());
            Ok(ControlPlaneEventStream::new(
                Box::pin(stream::pending()),
                activity_rx,
            ))
        })]);
        let mut subscriber = ControlPlaneSubscriber::new(client(), subscription, false);

        // The caller deadline (10s) is well inside EVENT_READ_TIMEOUT (60s), so this must fail
        // with the caller's own deadline rather than waiting out the staleness watchdog.
        let deadline = Instant::now() + Duration::from_secs(10);
        let started = Instant::now();
        let result = subscriber.next(Some(deadline)).await;
        assert!(matches!(
            result,
            Err(SessionsManagerClientError::OperationTimeout)
        ));
        assert!(started.elapsed() < Duration::from_secs(60));
    }
}
