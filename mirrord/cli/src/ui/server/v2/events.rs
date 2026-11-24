//! Cross-session interception events, served under `/api/v2/operator/events`.
//!
//! The operator streams every session at once when asked for no particular key, so this is one
//! upstream stream forwarded as one SSE response, payloads verbatim.

use std::time::Duration;

use axum::{
    extract::{Query, State},
    response::{Response, sse},
};
use futures::StreamExt;
use mirrord_operator::crd::NewOperatorFeature;
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::{AppState, OperatorFetchError, OperatorStatusSummary, cached_client, fetch_operator};
use crate::{
    subscribe::{EventStreamOptions, operator_event_stream},
    ui::server::{SseSender, sse_channel, sse_response},
};

/// How long to wait before reading the operator again, after a read failed or a stream ended.
const RETRY_INTERVAL: Duration = Duration::from_secs(5);

/// Name of the SSE event carrying [`Reachability`].
const STATUS_EVENT: &str = "status";

/// Whether the operator can be read for a context, and whether it can serve this view.
#[derive(PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Reachability {
    available: bool,

    /// Whether the operator advertises [`NewOperatorFeature::SubscribeEventOptions`].
    supported: bool,

    /// The installed operator version.
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

impl Reachability {
    fn of(operator: &Result<OperatorStatusSummary, OperatorFetchError>) -> Self {
        match operator {
            Ok(operator) => Self {
                available: true,
                supported: operator
                    .supported_features
                    .contains(&NewOperatorFeature::SubscribeEventOptions),
                version: Some(operator.version.clone()),
                reason: None,
            },
            Err(error) => Self {
                available: false,
                supported: false,
                version: None,
                reason: Some(error.to_string()),
            },
        }
    }
}

/// Query for [`operator_events_sse`].
#[derive(Deserialize)]
pub(super) struct EventsQuery {
    /// Context to stream from. Absent means the kubeconfig's current context.
    pub context: Option<String>,

    /// Also deliver queue messages that matched no session's filter.
    #[serde(default)]
    pub unmatched: bool,

    /// The namespace whose events to stream. Absent means every namespace that the kube user may
    /// see, which is what clients that never send it, such as the browser extension, expect.
    pub namespace: Option<String>,
}

/// Serves `GET /api/v2/operator/events`, streaming every live session's interception events for
/// `context`, in `namespace` when the query names one.
pub(super) async fn operator_events_sse(
    State(state): State<AppState>,
    Query(query): Query<EventsQuery>,
) -> Response {
    let (tx, rx) = sse_channel();

    tokio::spawn(forward(state, query, tx));

    sse_response(rx)
}

/// Reports reachability whenever it changes and forwards the operator's events, reopening the
/// stream when it ends, until the subscriber goes away.
async fn forward(state: AppState, query: EventsQuery, tx: SseSender) {
    let context = query.context.as_deref();
    let mut reported = None;

    loop {
        let status = Reachability::of(&fetch_operator(&state, context).await);
        let supported = status.supported;

        if reported.as_ref() != Some(&status) {
            let event = sse::Event::default()
                .event(STATUS_EVENT)
                .json_data(&status)
                .expect("reachability serialization cannot fail");

            if tx.send(Ok(event)).await.is_err() {
                return;
            }

            reported = Some(status);
        }

        if supported {
            stream_events(&state, &query, &tx).await;
        }

        tokio::select! {
            _ = tokio::time::sleep(RETRY_INTERVAL) => {}
            _ = tx.closed() => return,
        }
    }
}

/// Forwards the events of every session in `query.namespace` (every namespace when it's absent)
/// until the operator closes the stream or the subscriber disconnects.
async fn stream_events(state: &AppState, query: &EventsQuery, tx: &SseSender) {
    let context = query.context.as_deref();
    let client = match cached_client(state, context).await {
        Ok(client) => client,
        Err(error) => {
            warn!(?context, %error, "failed to build a kube client for the event stream");
            return;
        }
    };
    let options = EventStreamOptions {
        session_key_field: true,
        unmatched: query.unmatched,
    };
    let namespace = query.namespace.as_deref();
    let events = match operator_event_stream(&client, None, namespace, options).await {
        Ok(events) => events,
        Err(error) => {
            warn!(?context, %error, "failed to open the operator event stream");
            return;
        }
    };
    let mut events = std::pin::pin!(events);

    loop {
        let payload = tokio::select! {
            payload = events.next() => payload,
            _ = tx.closed() => return,
        };

        match payload {
            Some(Ok(payload)) => {
                if tx
                    .send(Ok(sse::Event::default().data(payload)))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Some(Err(error)) => {
                warn!(?context, %error, "the operator event stream failed");
                return;
            }
            None => return,
        }
    }
}
