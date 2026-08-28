use std::time::Duration;

use futures::StreamExt;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::{
    control_plane::{ControlPlaneEvent, ControlPlaneEventStream, HttpControlPlaneClient},
    error::SessionsManagerClientError,
    retry::{RetryDelays, init_retry_policy, run_interruptible, wait_next_retry_delay},
};

/// How long a connection may go without any activity — including SSE keep-alive frames, which
/// never surface as a decoded event — before it's considered stalled. See
/// [`ControlPlaneEventStream::last_activity`].
const EVENT_READ_TIMEOUT: Duration = Duration::from_secs(60);

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

pub(crate) struct ControlPlaneSubscriber<S> {
    client: HttpControlPlaneClient,
    subscription: S,
    cancellation: CancellationToken,
    events: Option<ControlPlaneEventStream>,
    retry_delays: RetryDelays,
    retry_initialization: bool,
    opened_once: bool,
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
            client,
            subscription,
            cancellation,
            events: None,
            retry_delays: init_retry_policy(),
            retry_initialization,
            opened_once: false,
            terminal: false,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn reset(&mut self) {
        self.events = None;
        self.terminal = false;
        self.retry_delays = init_retry_policy();
    }

    pub(crate) async fn next(&mut self) -> Option<Result<S::Output, SessionsManagerClientError>> {
        self.next_with_deadline(None).await
    }

    /// Waits for the next event, failing with [`SessionsManagerClientError::OperationTimeout`]
    /// once `deadline` passes.
    ///
    /// Within that budget, a connection that goes silent for [`EVENT_READ_TIMEOUT`] — including
    /// SSE keep-alives, not just decoded events — is treated the same as any other transport
    /// error: the subscription reconnects and keeps trying against the remaining budget, rather
    /// than blocking on a connection that's actually dead until `deadline` itself expires. This
    /// makes callers like `crate::client::intproxy`'s bounded connect flow noticeably more
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
        if self.terminal || self.cancellation.is_cancelled() {
            return None;
        }

        loop {
            if self.events.is_none() {
                tracing::debug!(
                    subscription = self.subscription.name(),
                    ?deadline,
                    opened_once = self.opened_once,
                    "opening sessions-manager control-plane subscription"
                );
                let stream = match run_interruptible(
                    &self.cancellation,
                    deadline,
                    self.subscription
                        .subscribe(&self.client, &self.cancellation),
                )
                .await
                {
                    Ok(stream) => stream,
                    Err(SessionsManagerClientError::Cancelled) => return None,
                    Err(error) => return Some(Err(error)),
                };

                match stream {
                    Ok(stream) => {
                        tracing::debug!(
                            subscription = self.subscription.name(),
                            "sessions-manager control-plane subscription opened"
                        );
                        self.events = Some(stream);
                        self.opened_once = true;
                    }
                    Err(error) => {
                        let can_retry = self.opened_once || self.retry_initialization;
                        if let Some(Err(error)) =
                            self.handle_error(error, can_retry, deadline).await
                        {
                            return Some(Err(error));
                        }
                        continue;
                    }
                }
            }

            let event = {
                let events = self
                    .events
                    .as_mut()
                    .expect("control-plane subscriber opened an event stream");
                match wait_for_event(events, &self.cancellation, deadline).await {
                    Ok(event) => event,
                    Err(SessionsManagerClientError::Cancelled) => return None,
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            subscription = self.subscription.name(),
                            ?deadline,
                            "sessions-manager control-plane event wait failed"
                        );
                        if let Some(Err(error)) = self.handle_error(error, true, deadline).await {
                            return Some(Err(error));
                        }
                        continue;
                    }
                }
            };

            match event {
                Some(Ok(event)) => match self.subscription.extract(event) {
                    Ok(output) => {
                        self.retry_delays = init_retry_policy();
                        return Some(Ok(output));
                    }
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            subscription = self.subscription.name(),
                            "sessions-manager control-plane subscription returned a terminal event"
                        );
                        self.terminal = true;
                        return Some(Err(error));
                    }
                },
                Some(Err(error)) => {
                    if let Some(Err(error)) = self.handle_error(error, true, deadline).await {
                        return Some(Err(error));
                    }
                }
                None => {
                    let error = SessionsManagerClientError::Sse(format!(
                        "{} stream ended",
                        self.subscription.name()
                    ));
                    if let Some(Err(error)) = self.handle_error(error, true, deadline).await {
                        return Some(Err(error));
                    }
                }
            }
        }
    }

    async fn handle_error(
        &mut self,
        error: SessionsManagerClientError,
        can_retry: bool,
        deadline: Option<Instant>,
    ) -> Option<Result<(), SessionsManagerClientError>> {
        self.events = None;
        if self.cancellation.is_cancelled() {
            return None;
        }
        if !error.is_retryable() || !can_retry {
            self.terminal = true;
            return Some(Err(error));
        }

        match wait_next_retry_delay(&mut self.retry_delays, &self.cancellation, deadline).await {
            Ok(retry_delay) => {
                tracing::warn!(
                    %error,
                    ?retry_delay,
                    subscription = self.subscription.name(),
                    "sessions-manager control-plane subscription failed"
                );
                Some(Ok(()))
            }
            Err(SessionsManagerClientError::Cancelled) => None,
            Err(error) => Some(Err(error)),
        }
    }
}

/// Waits for the next raw stream item, folding the caller's `deadline` (if any) and
/// [`EVENT_READ_TIMEOUT`] since the connection's last activity into a single wake condition.
///
/// Tracking last *activity* rather than last *decoded event* matters: an SSE keep-alive frame
/// never surfaces as a [`ControlPlaneEvent`], so if this only watched decoded events, a
/// perfectly healthy connection that's merely idle (no assignment ready yet) would be
/// indistinguishable from a silently dead one. See [`ControlPlaneEventStream::last_activity`].
///
/// A timed-out wait is returned as a plain [`SessionsManagerClientError::OperationTimeout`]
/// regardless of which condition fired; the caller treats it like any other transport error and
/// retries via [`ControlPlaneSubscriber::handle_error`], which itself respects `deadline`. That
/// means a real caller-deadline expiry still fails promptly (the retry wait immediately
/// re-times-out against the same expired deadline), while a `deadline`-less caller like
/// [`ControlPlaneSubscriber::next`] retries indefinitely — no branching on `deadline` is needed
/// here to get both behaviors right.
async fn wait_for_event(
    events: &mut ControlPlaneEventStream,
    cancellation: &CancellationToken,
    deadline: Option<Instant>,
) -> Result<Option<Result<ControlPlaneEvent, SessionsManagerClientError>>, SessionsManagerClientError>
{
    loop {
        let stale_at = events.last_activity() + EVENT_READ_TIMEOUT;
        let wake_at = deadline.map_or(stale_at, |deadline| deadline.min(stale_at));

        tokio::select! {
            _ = cancellation.cancelled() => return Err(SessionsManagerClientError::Cancelled),
            event = events.next() => return Ok(event),
            _ = tokio::time::sleep_until(wake_at) => {
                let now = Instant::now();
                let deadline_elapsed = deadline.is_some_and(|deadline| now >= deadline);
                let stalled = now >= events.last_activity() + EVENT_READ_TIMEOUT;
                if deadline_elapsed || stalled {
                    tracing::warn!(
                        ?deadline,
                        deadline_elapsed,
                        stalled,
                        last_activity = ?events.last_activity(),
                        "sessions-manager control-plane event stream timed out"
                    );
                    return Err(SessionsManagerClientError::OperationTimeout);
                }
                // Neither condition holds yet: a heartbeat pushed `stale_at` out further while
                // `deadline` (if any) is still ahead. Recompute and keep waiting.
            }
        }
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
