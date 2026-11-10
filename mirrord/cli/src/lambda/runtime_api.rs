//! The subset of the Lambda Runtime API a runtime interface client uses, served to the user's
//! command on loopback.
//!
//! Invocations wait in arrival order and the runtime takes them one at a time with `/next`, as in
//! Lambda. Each one is answered through the [`oneshot`] its invocation request waits on, so a
//! request that gave up (its remaining time ran out) leaves a closed channel behind, and the
//! invocation is dropped instead of being delivered.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use tokio::sync::{Notify, oneshot};

/// One invocation, with what the runtime needs to build the handler's context.
#[derive(Debug)]
pub(super) struct Invocation {
    pub request_id: String,
    pub deadline: Instant,
    /// The deadline as Lambda reports it, in milliseconds since the Unix epoch.
    pub deadline_epoch_ms: u128,
    pub function_arn: String,
    pub trace_id: Option<String>,
    /// JSON, as the Runtime API carries it (the Invoke API carries it base64 encoded).
    pub client_context: Option<String>,
    pub cognito_identity: Option<String>,
    pub event: Bytes,
}

/// How an invocation ended.
#[derive(Debug, Clone)]
pub(super) enum Outcome {
    Response(Bytes),
    Error(FunctionError),
}

/// An error the runtime reported, for one invocation or for its initialization.
#[derive(Debug, Clone)]
pub(super) struct FunctionError {
    /// The runtime's `Lambda-Runtime-Function-Error-Type`.
    pub error_type: String,
    /// The runtime's error document.
    pub body: Bytes,
}

impl FunctionError {
    /// The error for an invocation whose remaining time ran out before the local runtime answered.
    pub(super) fn local_timeout(request_id: &str) -> Self {
        let body = serde_json::json!({
            "errorType": "Mirrord.LocalTimeout",
            "errorMessage": format!(
                "Invocation {request_id} timed out before the local runtime answered it"
            ),
        });

        Self {
            error_type: "Mirrord.LocalTimeout".to_owned(),
            body: body.to_string().into(),
        }
    }
}

struct Pending {
    invocation: Invocation,
    reply: oneshot::Sender<Outcome>,
}

#[derive(Default)]
struct Queue {
    queue: VecDeque<Pending>,
    in_flight: HashMap<String, oneshot::Sender<Outcome>>,
    /// Set once the runtime reports a failed initialization. Lambda fails every invocation in an
    /// environment whose initialization failed, and so does the adapter.
    init_error: Option<FunctionError>,
}

#[derive(Default)]
pub(crate) struct Invocations {
    state: Mutex<Queue>,
    queued: Notify,
}

impl Invocations {
    /// Queues an invocation for the runtime, or fails it right away when the runtime failed to
    /// initialize.
    pub(super) fn enqueue(
        &self,
        invocation: Invocation,
    ) -> Result<oneshot::Receiver<Outcome>, FunctionError> {
        let mut state = self.state.lock().expect("invocations lock poisoned");
        if let Some(init_error) = &state.init_error {
            return Err(init_error.clone());
        }

        let (reply, outcome) = oneshot::channel();
        state.queue.push_back(Pending { invocation, reply });
        self.queued.notify_one();

        Ok(outcome)
    }

    /// The next invocation still waiting for an answer, once there is one.
    async fn next(&self) -> Invocation {
        loop {
            {
                let mut state = self.state.lock().expect("invocations lock poisoned");
                while let Some(Pending { invocation, reply }) = state.queue.pop_front() {
                    if reply.is_closed() {
                        tracing::debug!(
                            request_id = invocation.request_id,
                            "Dropping an invocation nobody waits for anymore"
                        );
                        continue;
                    }

                    state.in_flight.insert(invocation.request_id.clone(), reply);
                    return invocation;
                }
            }

            self.queued.notified().await;
        }
    }

    /// Answers an in-flight invocation. `false` when the runtime answers one it wasn't given.
    fn complete(&self, request_id: &str, outcome: Outcome) -> bool {
        let reply = self
            .state
            .lock()
            .expect("invocations lock poisoned")
            .in_flight
            .remove(request_id);

        match reply {
            Some(reply) => {
                // The invocation request may have given up on its remaining time already.
                let _ = reply.send(outcome);
                true
            }
            None => false,
        }
    }

    /// Fails every invocation, queued or in flight, and every later one, with the runtime's
    /// initialization error.
    fn fail_init(&self, error: FunctionError) {
        let mut state = self.state.lock().expect("invocations lock poisoned");
        let state = &mut *state;

        let queued = state.queue.drain(..).map(|pending| pending.reply);
        let in_flight = state.in_flight.drain().map(|(_, reply)| reply);
        for reply in queued.chain(in_flight) {
            let _ = reply.send(Outcome::Error(error.clone()));
        }

        state.init_error = Some(error);
    }
}

pub(super) fn router(invocations: Arc<Invocations>) -> Router {
    Router::new()
        .route("/2018-06-01/runtime/invocation/next", get(next))
        .route(
            "/2018-06-01/runtime/invocation/{request_id}/response",
            post(response),
        )
        .route(
            "/2018-06-01/runtime/invocation/{request_id}/error",
            post(error),
        )
        .route("/2018-06-01/runtime/init/error", post(init_error))
        .with_state(invocations)
}

async fn next(State(invocations): State<Arc<Invocations>>) -> Response {
    let invocation = invocations.next().await;
    tracing::debug!(
        request_id = invocation.request_id,
        "Delivering an invocation to the local runtime"
    );

    let mut headers = HeaderMap::new();
    let mut insert = |name: &'static str, value: &str| match HeaderValue::from_str(value) {
        Ok(value) => {
            headers.insert(HeaderName::from_static(name), value);
        }
        Err(error) => tracing::debug!(name, %error, "Dropping an invalid Runtime API header"),
    };

    insert("lambda-runtime-aws-request-id", &invocation.request_id);
    insert(
        "lambda-runtime-deadline-ms",
        &invocation.deadline_epoch_ms.to_string(),
    );
    insert(
        "lambda-runtime-invoked-function-arn",
        &invocation.function_arn,
    );
    if let Some(trace_id) = &invocation.trace_id {
        insert("lambda-runtime-trace-id", trace_id);
    }
    if let Some(client_context) = &invocation.client_context {
        insert("lambda-runtime-client-context", client_context);
    }
    if let Some(cognito_identity) = &invocation.cognito_identity {
        insert("lambda-runtime-cognito-identity", cognito_identity);
    }

    (StatusCode::OK, headers, invocation.event).into_response()
}

async fn response(
    State(invocations): State<Arc<Invocations>>,
    Path(request_id): Path<String>,
    body: Bytes,
) -> Response {
    accepted(invocations.complete(&request_id, Outcome::Response(body)))
}

async fn error(
    State(invocations): State<Arc<Invocations>>,
    Path(request_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let error = FunctionError {
        error_type: error_type(&headers),
        body,
    };
    accepted(invocations.complete(&request_id, Outcome::Error(error)))
}

async fn init_error(
    State(invocations): State<Arc<Invocations>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let error = FunctionError {
        error_type: error_type(&headers),
        body,
    };

    // The developer's handler failing to load is the one failure here they have to act on, and
    // the runtime interface client doesn't always print it.
    eprintln!(
        "mirrord lambda runtime: the local runtime failed to initialize ({}): {}",
        error.error_type,
        String::from_utf8_lossy(&error.body)
    );
    invocations.fail_init(error);

    accepted(true)
}

/// The runtime's `Lambda-Runtime-Function-Error-Type`, or `Unhandled`, the type Lambda's Invoke
/// API reports for a runtime error, when it sends none.
fn error_type(headers: &HeaderMap) -> String {
    headers
        .get("lambda-runtime-function-error-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("Unhandled")
        .to_owned()
}

fn accepted(known: bool) -> Response {
    if known {
        (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "status": "OK" })),
        )
            .into_response()
    } else {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "errorType": "InvalidRequestID",
                "errorMessage": "The request ID is not an invocation the runtime was given",
            })),
        )
            .into_response()
    }
}

/// The deadline for an invocation with `remaining` time left, both as an [`Instant`] and as Lambda
/// reports it to the runtime.
///
/// Built from the remaining time rather than the function's absolute deadline, so clock skew
/// between the execution environment and this machine doesn't matter.
pub(super) fn deadline(remaining: Duration) -> (Instant, u128) {
    let epoch_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .saturating_add(remaining)
        .as_millis();
    (Instant::now() + remaining, epoch_ms)
}
