//! HTTP/SSE control-plane transport.

mod api;
mod event;
pub(crate) mod subscriber;

use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

pub(crate) use api::AssignmentSubscription;
use api::{ControlPlaneApi, ControlPlaneEndpoint};
pub(crate) use event::ControlPlaneEvent;
use eventsource_stream::Eventsource;
use futures::{Stream, StreamExt};
use tokio::{sync::watch, time::Instant};
use tokio_util::sync::CancellationToken;

use crate::{
    config::SessionsManagerConfig, credentials::CredentialProvider,
    error::SessionsManagerClientError, retry::run_interruptible,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(30);

/// Tracks how recently any bytes were read off the socket, including SSE keep-alive comment
/// frames. `eventsource-stream` discards those internally before they'd ever surface as a
/// decoded item (an event only dispatches once its `data:` field is non-empty), so this is the
/// only point where they're observable at all — subscribers that need to distinguish a stalled
/// connection from one that's alive but has nothing to say yet rely on this.
struct ControlPlaneBytesStream {
    inner: Pin<Box<dyn Stream<Item = reqwest::Result<bytes::Bytes>> + Send>>,
    last_activity: watch::Sender<Instant>,
}

impl ControlPlaneBytesStream {
    fn new(
        inner: impl Stream<Item = reqwest::Result<bytes::Bytes>> + Send + 'static,
    ) -> (Self, watch::Receiver<Instant>) {
        let (last_activity, rx) = watch::channel(Instant::now());
        (
            Self {
                inner: Box::pin(inner),
                last_activity,
            },
            rx,
        )
    }
}

impl Stream for ControlPlaneBytesStream {
    type Item = reqwest::Result<bytes::Bytes>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let poll = this.inner.as_mut().poll_next(cx);
        if matches!(poll, Poll::Ready(Some(_))) {
            let _ = this.last_activity.send(Instant::now());
        }
        poll
    }
}

/// Decoded control-plane events, paired with a liveness signal ([`ControlPlaneBytesStream`])
/// that a caller waiting indefinitely for the next event can use to tell a stalled connection
/// apart from one that's alive but has nothing to say yet.
pub(crate) struct ControlPlaneEventStream {
    events:
        Pin<Box<dyn Stream<Item = Result<ControlPlaneEvent, SessionsManagerClientError>> + Send>>,
    last_activity: watch::Receiver<Instant>,
}

impl ControlPlaneEventStream {
    #[cfg(test)]
    pub(crate) fn new(
        events: Pin<
            Box<dyn Stream<Item = Result<ControlPlaneEvent, SessionsManagerClientError>> + Send>,
        >,
        last_activity: watch::Receiver<Instant>,
    ) -> Self {
        Self {
            events,
            last_activity,
        }
    }

    pub(crate) fn last_activity(&self) -> Instant {
        *self.last_activity.borrow()
    }
}

impl Stream for ControlPlaneEventStream {
    type Item = Result<ControlPlaneEvent, SessionsManagerClientError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().events.as_mut().poll_next(cx)
    }
}

#[derive(Clone)]
pub(crate) struct HttpControlPlaneClient {
    client: reqwest::Client,
    api: ControlPlaneApi,
    config: SessionsManagerConfig,
    credentials: Arc<dyn CredentialProvider>,
}

impl HttpControlPlaneClient {
    pub(crate) fn new(
        config: &SessionsManagerConfig,
        credentials: Arc<dyn CredentialProvider>,
    ) -> Result<Self, SessionsManagerClientError> {
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()?;
        Ok(Self {
            client,
            api: ControlPlaneApi::new(config.base_url.clone()),
            config: config.clone(),
            credentials,
        })
    }

    pub(crate) async fn subscribe_assignments(
        &self,
        subscription: &AssignmentSubscription,
        cancellation: &CancellationToken,
    ) -> Result<ControlPlaneEventStream, SessionsManagerClientError> {
        let endpoint = self.api.endpoint(ControlPlaneEndpoint::Assignments {
            environment: &self.config.environment,
            service: &self.config.service,
        })?;
        tracing::debug!(
            %endpoint,
            subscription = ?subscription,
            "requesting sessions-manager assignments"
        );
        let request = self
            .client
            .get(endpoint)
            .query(subscription)
            .headers(self.credentials.headers()?)
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send();
        let deadline = Instant::now() + RESPONSE_HEADER_TIMEOUT;
        let response = run_interruptible(cancellation, Some(deadline), request).await??;
        tracing::debug!(
            status = %response.status(),
            content_type = ?response.headers().get(reqwest::header::CONTENT_TYPE),
            "sessions-manager assignments response received"
        );
        if !response.status().is_success() {
            return Err(SessionsManagerClientError::HttpStatus(response.status()));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok());
        if !content_type.is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"))
        }) {
            return Err(SessionsManagerClientError::InvalidContentType(
                content_type.map(str::to_owned),
            ));
        }

        let (bytes, last_activity) = ControlPlaneBytesStream::new(response.bytes_stream());

        let api = self.api.clone();
        let events = Box::pin(bytes.eventsource().filter_map(move |event| {
            let api = api.clone();
            async move {
                match event {
                    Ok(event) => api.decode_event(event).transpose(),
                    Err(error) => Some(Err(SessionsManagerClientError::Sse(error.to_string()))),
                }
            }
        }));

        Ok(ControlPlaneEventStream {
            events,
            last_activity,
        })
    }
}
