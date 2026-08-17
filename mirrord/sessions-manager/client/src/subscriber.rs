use futures::StreamExt;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::{
    control_plane::{ControlPlaneEvent, ControlPlaneEventStream, HttpControlPlaneClient},
    error::{RetryDisposition, SessionsManagerClientError},
    retry::{RetryDelays, init_retry_policy, run_interruptible, wait_retry},
};

pub(crate) trait ControlPlaneSubscription {
    type Output;

    fn name(&self) -> &'static str;

    async fn subscribe(
        &self,
        client: &HttpControlPlaneClient,
        cancellation: &CancellationToken,
    ) -> Result<ControlPlaneEventStream, SessionsManagerClientError>;

    fn extract(&self, event: ControlPlaneEvent) -> Option<Self::Output>;
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

    pub(crate) async fn next(&mut self) -> Option<Result<S::Output, SessionsManagerClientError>> {
        self.next_with_deadline(None).await
    }

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
                match run_interruptible(&self.cancellation, deadline, events.next()).await {
                    Ok(event) => event,
                    Err(SessionsManagerClientError::Cancelled) => return None,
                    Err(error) => return Some(Err(error)),
                }
            };

            match event {
                Some(Ok(event)) => {
                    if let Some(output) = self.subscription.extract(event) {
                        self.retry_delays = init_retry_policy();
                        return Some(Ok(output));
                    }
                }
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
        if error.retry_disposition() != RetryDisposition::Retry || !can_retry {
            self.terminal = true;
            return Some(Err(error));
        }

        let retry_delay = self
            .retry_delays
            .next()
            .expect("exponential backoff strategy is unbounded");
        tracing::warn!(
            %error,
            ?retry_delay,
            subscription = self.subscription.name(),
            "sessions-manager control-plane subscription failed"
        );

        match wait_retry(&self.cancellation, deadline, retry_delay).await {
            Ok(()) => Some(Ok(())),
            Err(SessionsManagerClientError::Cancelled) => None,
            Err(error) => Some(Err(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Arc, time::Duration};

    use futures::stream;
    use tokio::{sync::Mutex, time::Instant};
    use tokio_util::sync::CancellationToken;
    use url::Url;

    use super::{ControlPlaneSubscriber, ControlPlaneSubscription};
    use crate::{
        config::SessionsManagerConfig,
        control_plane::{ControlPlaneEvent, ControlPlaneEventStream, HttpControlPlaneClient},
        credentials::NoCredentials,
        error::SessionsManagerClientError,
    };

    struct TestSubscription {
        streams: Mutex<VecDeque<Result<ControlPlaneEventStream, SessionsManagerClientError>>>,
    }

    impl TestSubscription {
        fn new(streams: Vec<Result<ControlPlaneEventStream, SessionsManagerClientError>>) -> Self {
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
            self.streams
                .lock()
                .await
                .pop_front()
                .expect("test configured another stream result")
        }

        fn extract(&self, event: ControlPlaneEvent) -> Option<Self::Output> {
            match event {
                ControlPlaneEvent::Assignment(_) => Some(()),
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
                "data_plane_endpoint": "/ws/test",
                "authorization": "Bearer test",
            }),
        )?))
    }

    fn events(
        events: Vec<Result<ControlPlaneEvent, SessionsManagerClientError>>,
    ) -> ControlPlaneEventStream {
        Box::pin(stream::iter(events))
    }

    #[tokio::test]
    async fn retries_initial_open_when_enabled() {
        let subscription = TestSubscription::new(vec![
            Err(SessionsManagerClientError::Sse(
                "temporary failure".to_owned(),
            )),
            Ok(events(vec![assignment_event()])),
        ]);
        let mut subscriber =
            ControlPlaneSubscriber::new(client(), subscription, CancellationToken::new(), true);

        assert!(matches!(subscriber.next().await, Some(Ok(()))));
    }

    #[tokio::test]
    async fn reopens_after_a_successful_stream_ends() {
        let subscription = TestSubscription::new(vec![
            Ok(events(Vec::new())),
            Ok(events(vec![assignment_event()])),
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

    #[tokio::test]
    async fn does_not_retry_initial_open_when_disabled() {
        let subscription = TestSubscription::new(vec![Err(SessionsManagerClientError::Sse(
            "temporary failure".to_owned(),
        ))]);
        let mut subscriber =
            ControlPlaneSubscriber::new(client(), subscription, CancellationToken::new(), false);

        assert!(matches!(
            subscriber
                .next_until(Instant::now() + Duration::from_secs(1))
                .await,
            Err(SessionsManagerClientError::Sse(_))
        ));
    }
}
