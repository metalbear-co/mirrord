//! Transport-agnostic decoding of the control-plane SSE stream.

pub(crate) mod api;
mod event;
pub(crate) mod subscriber;

use std::{
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use api::ControlPlaneApi;
pub(crate) use event::ControlPlaneEvent;
use eventsource_stream::{EventStreamError, Eventsource};
use futures::{Stream, StreamExt};
use hyper::{
    StatusCode,
    header::{CONTENT_TYPE, HeaderMap},
};
use tokio::{sync::watch, time::Instant};

use crate::error::SessionsManagerClientError;

/// How long a connection may go without any activity before it's considered stalled. See
/// [`ControlPlaneEventStream::next`].
const EVENT_READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Tracks how recently any bytes were read off the socket, including SSE keep-alive comment
/// frames. `eventsource-stream` discards those internally before they'd ever surface as a
/// decoded item (an event only dispatches once its `data:` field is non-empty), so this is the
/// only point where they're observable at all — subscribers that need to distinguish a stalled
/// connection from one that's alive but has nothing to say yet rely on this.
struct ControlPlaneBytesStream<S> {
    inner: Pin<Box<S>>,
    last_activity: watch::Sender<Instant>,
}

impl<S> ControlPlaneBytesStream<S> {
    fn new(inner: S) -> (Self, watch::Receiver<Instant>) {
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

impl<S: Stream> Stream for ControlPlaneBytesStream<S> {
    type Item = S::Item;

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
///
/// Produced by a [`crate::SessionsManagerTransport`]; public only because it appears in that
/// trait's signature.
pub struct ControlPlaneEventStream {
    events:
        Pin<Box<dyn Stream<Item = Result<ControlPlaneEvent, SessionsManagerClientError>> + Send>>,
    last_activity: watch::Receiver<Instant>,
}

impl ControlPlaneEventStream {
    /// Decodes an SSE response body, whichever transport it arrived over.
    pub(crate) fn from_bytes<E>(
        bytes: impl Stream<Item = Result<bytes::Bytes, E>> + Send + 'static,
    ) -> Self
    where
        E: Send + 'static,
        SessionsManagerClientError: From<EventStreamError<E>>,
    {
        let (bytes, last_activity) = ControlPlaneBytesStream::new(bytes);
        let events = Box::pin(bytes.eventsource().filter_map(|event| async move {
            match event {
                Ok(event) => ControlPlaneApi::decode_event(event).transpose(),
                Err(error) => Some(Err(error.into())),
            }
        }));

        Self {
            events,
            last_activity,
        }
    }

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

    fn last_activity(&self) -> Instant {
        *self.last_activity.borrow()
    }

    /// Waits for the next event, failing with [`SessionsManagerClientError::OperationTimeout`] if
    /// no activity — including SSE keep-alives, which never surface as a decoded event — has been
    /// seen for [`EVENT_READ_TIMEOUT`].
    ///
    /// Only this stream's own liveness is guarded here; an overall caller deadline is the
    /// enclosing retry loop's job, not this stream's.
    pub(crate) async fn next(
        &mut self,
    ) -> Result<
        Option<Result<ControlPlaneEvent, SessionsManagerClientError>>,
        SessionsManagerClientError,
    > {
        loop {
            let stale_at = self.last_activity() + EVENT_READ_TIMEOUT;
            tokio::select! {
                event = self.events.next() => return Ok(event),
                _ = tokio::time::sleep_until(stale_at) => {
                    if Instant::now() >= self.last_activity() + EVENT_READ_TIMEOUT {
                        tracing::warn!(
                            last_activity = ?self.last_activity(),
                            "sessions-manager control-plane event stream timed out"
                        );
                        return Err(SessionsManagerClientError::OperationTimeout);
                    }
                    // A heartbeat pushed `stale_at` out further since we armed the sleep — loop
                    // back and recompute against the current `last_activity()`.
                }
            }
        }
    }
}

pub(crate) fn verify_event_stream(
    status: StatusCode,
    headers: &HeaderMap,
) -> Result<(), SessionsManagerClientError> {
    tracing::debug!(
        %status,
        content_type = ?headers.get(CONTENT_TYPE),
        "sessions-manager assignments response received"
    );
    if !status.is_success() {
        return Err(SessionsManagerClientError::HttpStatus(status));
    }

    let content_type = headers
        .get(CONTENT_TYPE)
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

    Ok(())
}

#[cfg(test)]
mod tests {
    use futures::stream;

    use super::*;

    /// The body `sessionassignments` streams, split across chunks the way a transport may
    /// deliver it, including a keep-alive comment and an event this client doesn't know.
    #[tokio::test]
    async fn operator_sse_body_decodes_into_assignment() {
        let chunks = [
            ": keep-alive\n\n",
            "event: unknown\ndata: {}\n\n",
            "event: assignment\ndata: {\"assignment_id\":\"assignment-1\",",
            "\"data_plane_endpoint\":\"/apis/operator.metalbear.co/v1alpha1/sessiondataplanes/assignment-1\",",
            "\"authorization\":\"Bearer secret\"}\n\n",
            "event: superseded\ndata: {}\n\n",
        ];
        let mut events = ControlPlaneEventStream::from_bytes(stream::iter(
            chunks.map(|chunk| Ok::<_, kube::Error>(bytes::Bytes::from(chunk))),
        ));

        let Some(Ok(ControlPlaneEvent::Assignment(assignment))) = events.next().await.unwrap()
        else {
            panic!("expected an assignment event");
        };
        assert_eq!(assignment.assignment_id.to_string(), "assignment-1");
        assert_eq!(
            assignment.data_plane_endpoint.as_str(),
            "/apis/operator.metalbear.co/v1alpha1/sessiondataplanes/assignment-1"
        );
        assert!(matches!(
            events.next().await.unwrap(),
            Some(Ok(ControlPlaneEvent::Superseded))
        ));
        assert!(events.next().await.unwrap().is_none());
    }
}
