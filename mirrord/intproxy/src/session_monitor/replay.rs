//! Captures the HTTP requests that the session receives for the local application, and replays
//! them.
//!
//! The incoming proxy captures each request in the session's [`ReplayStore`] when it starts the
//! HTTP gateway task of the request, and then tells the store about the rest of the body as it
//! arrives and about the answer of the local application. So a request is listed while the local
//! application still handles it, and it stays listed when the client that made it gives up. A
//! replay sends the captured request to the local application again, through the [`ClientStore`]
//! that delivers the requests from the agent, and returns the response to the caller of the
//! [`replay_router`] routes. Nothing about a replay is sent to the agent, so the client that made
//! the original request never sees it.
//!
//! Captured requests live in memory, and only while the session runs. The store keeps at most
//! [`MAX_KEPT_REQUESTS`] requests, [`MAX_KEPT_BODY_BYTES`] bytes of each body and
//! [`MAX_KEPT_REPLAYS`] replays of each request. It keeps headers whole.

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    extract::{Path, State},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::{
    HeaderMap, Method, StatusCode, Uri, Version,
    header::{
        ACCEPT_ENCODING, CONTENT_LENGTH, HeaderName, HeaderValue, TRANSFER_ENCODING, USER_AGENT,
    },
};
use mirrord_intproxy_protocol::ListeningOn;
use mirrord_protocol::{
    Payload, Port,
    tcp::{
        HttpRequest, IncomingTrafficTransportType, InternalHttpBodyFrame, InternalHttpRequest,
        is_sensitive_name,
    },
};
use mirrord_session_monitor_protocol::{
    CapturedBody, CapturedRequest, CapturedRequestSummary, CapturedResponse, NotReplayable,
    ReplayOptions, ReplayRun,
};
use thiserror::Error;
use tokio::time;
use tracing::Level;
use url::form_urlencoded;

use crate::{
    proxies::incoming::http::{ClientStore, LocalHttpError, StreamingBody, send_backoffs},
    session_monitor::api::AppState,
};

/// How many requests a session keeps. A new request replaces the oldest one.
const MAX_KEPT_REQUESTS: usize = 100;

/// How many bytes of a body a session keeps. A request with a larger body can't be replayed.
const MAX_KEPT_BODY_BYTES: usize = 32 * 1024;

/// How many replays of one request a session keeps. A new replay replaces the oldest one.
const MAX_KEPT_REPLAYS: usize = 5;

/// How long a replay waits for the next part of a response body.
///
/// A response such as an event stream never ends, and a replay must still return.
const RESPONSE_BODY_IDLE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a replay reads a response body in total.
///
/// A response can keep sending small parts more often than [`RESPONSE_BODY_IDLE_TIMEOUT`].
const RESPONSE_BODY_TIMEOUT: Duration = Duration::from_secs(30);

/// Replaces a value that usually carries a secret, in what the routes return.
const REDACTED: &str = "[REDACTED]";

/// Routes for `/replay/requests`.
///
/// - `GET /`: lists the captured requests, oldest first;
/// - `GET /{id}`: gets one captured request with its headers, body and replays;
/// - `POST /{id}/replay`: sends the request to the local application again and returns the
///   response. The body is an optional [`ReplayOptions`].
pub(super) fn replay_router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_requests))
        .route("/{id}", get(get_request))
        .route("/{id}/replay", post(replay_request))
}

#[tracing::instrument(level = Level::DEBUG, skip(state))]
async fn list_requests(State(state): State<AppState>) -> Json<Vec<CapturedRequestSummary>> {
    Json(state.replay_store.list())
}

#[tracing::instrument(level = Level::DEBUG, skip(state), err)]
async fn get_request(
    State(state): State<AppState>,
    Path(id): Path<u64>,
) -> Result<Json<CapturedRequest>, ReplayError> {
    state.replay_store.get(id).map(Json)
}

// The options hold header values, which can be secrets, so they stay out of the logs.
#[tracing::instrument(level = Level::DEBUG, skip(state, body), err)]
async fn replay_request(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    body: Bytes,
) -> Result<Json<ReplayRun>, ReplayError> {
    let options = replay_options(&body)?;

    state.replay_store.replay(id, options).await.map(Json)
}

/// Reads the options of a replay from the body of its request, whatever the content type,
/// because a client that sends options without one expects them to apply.
fn replay_options(body: &[u8]) -> Result<ReplayOptions, ReplayError> {
    if body.is_empty() {
        return Ok(ReplayOptions::default());
    }

    Json::<ReplayOptions>::from_bytes(body)
        .map(|Json(options)| options)
        .map_err(|rejection| ReplayError::InvalidOptions(rejection.body_text()))
}

/// Why a route of the [`replay_router`] refused or failed.
#[derive(Debug, Error)]
pub(crate) enum ReplayError {
    #[error("the session has no request with id {0}")]
    NotFound(u64),

    #[error("request {id} can't be replayed: {reason}")]
    NotReplayable { id: u64, reason: NotReplayable },

    #[error("the replay options are invalid: {0}")]
    InvalidOptions(String),

    #[error("header `{0}` has an invalid name or value")]
    InvalidHeader(String),

    #[error("header `{0}` can't be changed, because it describes the body")]
    BodyHeader(HeaderName),

    #[error("the local application is not listening for port {0}")]
    NotListening(Port),

    #[error("{}", with_causes(.0))]
    Send(#[from] LocalHttpError),
}

/// Returns the text of `error` and of each cause that the text doesn't already end with, because
/// a [`hyper::Error`] leaves its cause, such as a reset connection, out of its text.
fn with_causes(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !text.ends_with(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        source = cause.source();
    }

    text
}

impl IntoResponse for ReplayError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::NotFound(..) => StatusCode::NOT_FOUND,
            Self::NotReplayable { .. } => StatusCode::CONFLICT,
            Self::InvalidOptions(..) | Self::InvalidHeader(..) | Self::BodyHeader(..) => {
                StatusCode::BAD_REQUEST
            }
            Self::NotListening(..) | Self::Send(..) => StatusCode::BAD_GATEWAY,
        };

        (status, self.to_string()).into_response()
    }
}

/// How the captured request was sent to the local application, for a replay to send it the same
/// way.
#[derive(Clone)]
struct Delivery {
    client_store: ClientStore,
    transport: IncomingTrafficTransportType,
}

/// The start of a request body, which is what the store keeps of it.
#[derive(Default)]
struct KeptBody {
    /// The first [`MAX_KEPT_BODY_BYTES`] bytes of the body.
    ///
    /// A copy, because a frame can be a slice of a much larger buffer, such as the one that a
    /// message from the operator was read into, and sharing it would keep that buffer alive as
    /// long as the request is kept.
    data: Vec<u8>,

    trailers: Option<HeaderMap>,

    /// Size of the data that arrived so far.
    size: usize,

    /// Size of the whole body, when the request states it in its `content-length` header and
    /// can't send trailers after the body.
    stated_size: Option<usize>,

    /// Whether the frames that end the body arrived.
    ended: bool,
}

impl KeptBody {
    /// Adds the next `frames` of the body, where `is_last` says whether they end it.
    fn push(&mut self, frames: &[InternalHttpBodyFrame], is_last: bool) {
        for frame in frames {
            match frame {
                InternalHttpBodyFrame::Data(payload) => {
                    keep_start(&mut self.data, &payload.0);
                    self.size += payload.0.len();
                }
                InternalHttpBodyFrame::Trailers(headers) => {
                    self.trailers = Some(headers.clone());
                    self.ended = true;
                }
            }
        }

        self.ended |= is_last;
        if self.is_complete() {
            self.data.shrink_to_fit();
        }
    }

    /// Returns whether all of the body arrived.
    ///
    /// The agent sends the end of a body in a message of its own. That message can arrive after
    /// the local application answered, when the proxy no longer follows the request. So a body
    /// that has its stated size is complete without it, and so is a body whose trailers arrived,
    /// because trailers are always the last frame of a body.
    fn is_complete(&self) -> bool {
        self.ended || self.stated_size == Some(self.size)
    }

    fn not_replayable(&self) -> Option<NotReplayable> {
        if !self.is_complete() {
            Some(NotReplayable::BodyIncomplete)
        } else if self.size > MAX_KEPT_BODY_BYTES {
            Some(NotReplayable::BodyTooLarge)
        } else {
            None
        }
    }

    /// Returns the body as frames, for sending it again.
    fn to_frames(&self) -> Vec<InternalHttpBodyFrame> {
        let mut frames = vec![InternalHttpBodyFrame::Data(Payload(
            Bytes::copy_from_slice(&self.data),
        ))];
        frames.extend(self.trailers.clone().map(InternalHttpBodyFrame::Trailers));

        frames
    }

    fn to_captured(&self) -> CapturedBody {
        captured_body(&self.data, self.size, self.size > MAX_KEPT_BODY_BYTES)
    }
}

/// The start of a response body, as the store keeps it.
struct KeptResponseBody {
    /// The first [`MAX_KEPT_BODY_BYTES`] bytes of the body, as they arrived, because text with
    /// the bytes that aren't valid UTF-8 replaced takes up to three times as much memory.
    data: Vec<u8>,

    /// Size of the data that arrived.
    size: usize,

    /// Whether the replay stopped reading before the end of the body.
    truncated: bool,
}

impl KeptResponseBody {
    fn to_captured(&self) -> CapturedBody {
        captured_body(&self.data, self.size, self.truncated)
    }
}

/// A replay of a captured request, as the store keeps it.
struct KeptReplay {
    number: u32,

    at_ms: u64,

    response: CapturedResponse,

    body: KeptResponseBody,
}

impl KeptReplay {
    fn to_run(&self) -> ReplayRun {
        ReplayRun {
            number: self.number,
            at_ms: self.at_ms,
            response: self.response.clone(),
            body: self.body.to_captured(),
        }
    }
}

/// A captured request, as the store keeps it.
struct StoredRequest {
    id: u64,

    captured_at: SystemTime,

    is_steal: bool,

    port: Port,

    method: Method,

    uri: Uri,

    version: Version,

    headers: HeaderMap,

    body: KeptBody,

    /// How the local application answered the original request.
    response: Option<CapturedResponse>,

    /// How many replays of the request finished.
    ///
    /// Shared with the replays that wait for the local application, so that a replay that
    /// finishes after newer requests pushed the request out still gets a number of its own.
    replay_count: Arc<AtomicU32>,

    /// The latest replays, oldest first.
    replays: VecDeque<KeptReplay>,

    delivery: Delivery,
}

impl StoredRequest {
    fn not_replayable(&self) -> Option<NotReplayable> {
        let upgraded = self
            .response
            .as_ref()
            .is_some_and(|response| response.status == StatusCode::SWITCHING_PROTOCOLS.as_u16());
        if upgraded {
            return Some(NotReplayable::Upgraded);
        }

        self.body.not_replayable()
    }

    /// Returns the request to send for a replay, with `header_changes` applied.
    fn replay_request(
        &self,
        header_changes: Vec<(HeaderName, Option<HeaderValue>)>,
    ) -> HttpRequest<StreamingBody> {
        let mut headers = self.headers.clone();
        // The response body is shown as text, so the replay asks for one that isn't
        // compressed, unless the caller chose an encoding.
        headers.insert(ACCEPT_ENCODING, HeaderValue::from_static("identity"));
        for (header, value) in header_changes {
            match value {
                Some(value) => headers.insert(header, value),
                None => headers.remove(header),
            };
        }

        HttpRequest {
            internal_request: InternalHttpRequest {
                method: self.method.clone(),
                uri: self.uri.clone(),
                headers,
                version: self.version,
                body: StreamingBody::from(self.body.to_frames()),
            },
            // These identify the request to the agent, which never sees a replay.
            connection_id: 0,
            request_id: 0,
            port: self.port,
        }
    }

    fn summary(&self) -> CapturedRequestSummary {
        CapturedRequestSummary {
            id: self.id,
            captured_at_ms: unix_millis(self.captured_at),
            mode: if self.is_steal { "steal" } else { "mirror" }.to_owned(),
            port: self.port,
            method: self.method.to_string(),
            path: redacted_path(&self.uri),
            status: self.response.as_ref().map(|response| response.status),
            duration_ms: self.response.as_ref().map(|response| response.duration_ms),
            body_size: u64::try_from(self.body.size).unwrap_or(u64::MAX),
            not_replayable: self.not_replayable(),
            replay_count: self.replay_count.load(Ordering::Relaxed),
        }
    }

    fn detail(&self) -> CapturedRequest {
        CapturedRequest {
            summary: self.summary(),
            version: format!("{:?}", self.version),
            headers: header_pairs(&self.headers),
            body: self.body.to_captured(),
            response: self.response.clone(),
            replays: self.replays.iter().map(KeptReplay::to_run).collect(),
        }
    }
}

/// What a [`ReplayStore`] holds.
#[derive(Default)]
struct StoreState {
    /// Id of the latest captured request. Ids start at 1 and are never reused in a session.
    last_id: u64,

    /// Oldest first, so ordered by id.
    requests: VecDeque<StoredRequest>,

    /// Where the local application listens for each subscribed port.
    listeners: HashMap<Port, ListeningOn>,
}

impl StoreState {
    /// Returns the position of the request `id` in [`Self::requests`].
    fn index_of(&self, id: u64) -> Option<usize> {
        self.requests
            .binary_search_by_key(&id, |request| request.id)
            .ok()
    }

    fn request(&self, id: u64) -> Option<&StoredRequest> {
        self.requests.get(self.index_of(id)?)
    }

    fn request_mut(&mut self, id: u64) -> Option<&mut StoredRequest> {
        let index = self.index_of(id)?;

        self.requests.get_mut(index)
    }
}

/// The requests that a session captured. Clones share the same requests.
#[derive(Clone, Default)]
pub struct ReplayStore(Arc<Mutex<StoreState>>);

impl ReplayStore {
    fn state(&self) -> MutexGuard<'_, StoreState> {
        // A change that stops halfway, such as an id taken without its request, leaves the state
        // usable, so a poisoned lock is safe to use.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Replaces what the store knows about where the local application listens.
    pub(crate) fn set_listeners<'a>(
        &self,
        listeners: impl Iterator<Item = (Port, &'a ListeningOn)>,
    ) {
        self.state().listeners = listeners
            .map(|(port, listening_on)| (port, listening_on.clone()))
            .collect();
    }

    /// Captures a request that is about to be sent to the local application.
    ///
    /// `body_is_complete` says whether the request already has every frame of its body.
    /// Otherwise, the rest goes to [`CaptureHandle::body_arrived`].
    ///
    /// Returns [`None`] for a health probe of the kubelet, which the store doesn't capture.
    pub(crate) fn capture(
        &self,
        request: &HttpRequest<StreamingBody>,
        body_is_complete: bool,
        is_steal: bool,
        client_store: &ClientStore,
        transport: &IncomingTrafficTransportType,
    ) -> Option<CaptureHandle> {
        let InternalHttpRequest {
            method,
            uri,
            headers,
            version,
            body,
        } = &request.internal_request;

        // The kubelet probes the target every few seconds, so its requests would push the ones
        // worth replaying out of the store.
        let is_health_probe = headers
            .get(USER_AGENT)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|user_agent| user_agent.starts_with("kube-probe/"));
        if is_health_probe {
            return None;
        }

        // A chunked body and the body of an HTTP/2 request can be followed by trailers, so
        // their size doesn't say that all of the request arrived.
        let can_have_trailers =
            *version >= Version::HTTP_2 || headers.contains_key(TRANSFER_ENCODING);
        let mut kept_body = KeptBody {
            stated_size: headers
                .get(CONTENT_LENGTH)
                .filter(|_| !can_have_trailers)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse().ok()),
            ..Default::default()
        };
        body.buffered(|frames| kept_body.push(frames, body_is_complete));

        let mut state = self.state();

        state.last_id += 1;
        let id = state.last_id;

        if state.requests.len() >= MAX_KEPT_REQUESTS {
            state.requests.pop_front();
        }
        state.requests.push_back(StoredRequest {
            id,
            captured_at: SystemTime::now(),
            is_steal,
            port: request.port,
            method: method.clone(),
            uri: uri.clone(),
            version: *version,
            headers: headers.clone(),
            body: kept_body,
            response: None,
            replay_count: Default::default(),
            replays: VecDeque::new(),
            delivery: Delivery {
                client_store: client_store.clone(),
                transport: transport.clone(),
            },
        });

        Some(CaptureHandle {
            store: self.clone(),
            id,
        })
    }

    pub(crate) fn list(&self) -> Vec<CapturedRequestSummary> {
        self.state()
            .requests
            .iter()
            .map(StoredRequest::summary)
            .collect()
    }

    fn get(&self, id: u64) -> Result<CapturedRequest, ReplayError> {
        self.state()
            .request(id)
            .map(StoredRequest::detail)
            .ok_or(ReplayError::NotFound(id))
    }

    /// Sends the captured request `id` to the local application again, changed as `options` say,
    /// and returns the response.
    ///
    /// Waits for the response headers as long as the local application takes, so that a developer
    /// can hold the request on a breakpoint.
    pub(crate) async fn replay(
        &self,
        id: u64,
        options: ReplayOptions,
    ) -> Result<ReplayRun, ReplayError> {
        let mut header_changes = Vec::new();
        for (name, value) in options.headers {
            let invalid = || ReplayError::InvalidHeader(name.clone());

            let header = HeaderName::try_from(&name).map_err(|_| invalid())?;
            if header == CONTENT_LENGTH || header == TRANSFER_ENCODING {
                return Err(ReplayError::BodyHeader(header));
            }

            let value = value
                .map(|value| HeaderValue::try_from(value).map_err(|_| invalid()))
                .transpose()?;
            header_changes.push((header, value));
        }

        let (request, delivery, replay_count) = {
            let state = self.state();
            let stored = state.request(id).ok_or(ReplayError::NotFound(id))?;

            if let Some(reason) = stored.not_replayable() {
                return Err(ReplayError::NotReplayable { id, reason });
            }

            (
                stored.replay_request(header_changes),
                stored.delivery.clone(),
                stored.replay_count.clone(),
            )
        };

        let sent_at = SystemTime::now();
        let mut backoffs = send_backoffs();
        let (response, body) = loop {
            match self.attempt(&request, &delivery).await {
                Ok(response) => break response,
                Err(error) => match next_backoff(&error, &mut backoffs) {
                    Some(backoff) => time::sleep(backoff).await,
                    None => return Err(error),
                },
            }
        };

        let mut replay = KeptReplay {
            number: 0,
            at_ms: unix_millis(sent_at),
            response,
            body,
        };

        // Converted before taking the lock, which the incoming requests wait for.
        let mut run = replay.to_run();

        // Other replays of the request can finish first, so the number is taken when the replay
        // finishes, under the lock, so that the store keeps the replays in the order of their
        // numbers.
        let mut state = self.state();
        replay.number = replay_count.fetch_add(1, Ordering::Relaxed) + 1;
        run.number = replay.number;

        // Newer requests can push the request out of the store while its replay waits for the
        // local application. The caller still gets the response.
        let Some(stored) = state.request_mut(id) else {
            return Ok(run);
        };

        if stored.replays.len() >= MAX_KEPT_REPLAYS {
            stored.replays.pop_front();
        }
        stored.replays.push_back(replay);

        Ok(run)
    }

    /// Looks up where the local application listens at the time of the call, and makes one
    /// attempt to send `request` there with [`send_and_read`].
    async fn attempt(
        &self,
        request: &HttpRequest<StreamingBody>,
        delivery: &Delivery,
    ) -> Result<(CapturedResponse, KeptResponseBody), ReplayError> {
        // Read on every attempt, because a local application that restarts can come back on
        // another address.
        let listening_on = self
            .state()
            .listeners
            .get(&request.port)
            .cloned()
            .ok_or(ReplayError::NotListening(request.port))?;

        Ok(send_and_read(request, &listening_on, delivery).await?)
    }
}

/// Updates the request that [`ReplayStore::capture`] stored, while the request is being handled.
#[derive(Clone)]
pub(crate) struct CaptureHandle {
    store: ReplayStore,
    id: u64,
}

impl CaptureHandle {
    /// Tells the store that the next `frames` of the request body arrived, where `is_last` says
    /// whether they end it.
    pub(crate) fn body_arrived(&self, frames: &[InternalHttpBodyFrame], is_last: bool) {
        if let Some(stored) = self.store.state().request_mut(self.id) {
            stored.body.push(frames, is_last);
        }
    }

    /// Tells the store that an attempt to send the request starts.
    ///
    /// The store forgets the answer to the previous attempt, which the client never got.
    pub(crate) fn attempt_started(&self) {
        if let Some(stored) = self.store.state().request_mut(self.id) {
            stored.response = None;
        }
    }

    /// Tells the store how the local application answered the current attempt to send the
    /// request.
    pub(crate) fn responded(&self, status: StatusCode, headers: &HeaderMap, duration: Duration) {
        // Copied into text, because hyper's header values are slices of the read buffer of the
        // connection, and keeping them would keep that whole buffer allocated.
        let response = CapturedResponse {
            status: status.as_u16(),
            headers: header_pairs(headers),
            duration_ms: millis(duration),
        };

        if let Some(stored) = self.store.state().request_mut(self.id) {
            stored.response = Some(response);
        }
    }
}

/// Returns how long a replay waits before the next attempt to send its request after `error`,
/// taking the wait from `backoffs`, or [`None`] when there should be no next attempt.
///
/// There is a next attempt only when the local application can't have received the request,
/// because one replay must not make it handle the request twice. The HTTP gateway task retries
/// more failures than these.
fn next_backoff(
    error: &ReplayError,
    backoffs: &mut impl Iterator<Item = Duration>,
) -> Option<Duration> {
    match error {
        // The port has no subscription while the local application restarts.
        ReplayError::NotListening(..)
        | ReplayError::Send(
            LocalHttpError::ConnectTcpFailed(..) | LocalHttpError::ConnectTlsFailed(..),
        ) => backoffs.next(),

        // The pooled connection was gone before the request left, so the next attempt only needs
        // a new connection, not a wait.
        ReplayError::Send(error) if error.is_connection_closed() => {
            backoffs.next().map(|_| Duration::ZERO)
        }

        _ => None,
    }
}

/// Sends `request` to the local application at `listening_on` once, and reads the start of the
/// response body.
async fn send_and_read(
    request: &HttpRequest<StreamingBody>,
    listening_on: &ListeningOn,
    delivery: &Delivery,
) -> Result<(CapturedResponse, KeptResponseBody), LocalHttpError> {
    let sent_at = Instant::now();
    let (client, mut response) = delivery
        .client_store
        .send(listening_on, &delivery.transport, request.clone())
        .await?;
    let duration = sent_at.elapsed();

    let body_deadline = time::Instant::now() + RESPONSE_BODY_TIMEOUT;
    let mut data = Vec::new();
    let mut size = 0;
    let mut truncated = false;
    loop {
        let frame_deadline = body_deadline.min(time::Instant::now() + RESPONSE_BODY_IDLE_TIMEOUT);
        let Ok(frame) = time::timeout_at(frame_deadline, response.body_mut().frame()).await else {
            truncated = true;
            break;
        };
        let Some(frame) = frame else {
            break;
        };

        let frame = frame.map_err(LocalHttpError::ReadBodyFailed)?;
        let Some(chunk) = frame.data_ref() else {
            continue;
        };

        size += chunk.len();
        keep_start(&mut data, chunk);

        if size > MAX_KEPT_BODY_BYTES {
            truncated = true;
            break;
        }
    }

    // Only a connection whose response was read to its end can serve another request. An upgraded
    // connection no longer speaks HTTP.
    if !truncated && response.status() != StatusCode::SWITCHING_PROTOCOLS {
        delivery.client_store.push_idle(client);
    }

    let head = CapturedResponse {
        status: response.status().as_u16(),
        headers: header_pairs(response.headers()),
        duration_ms: millis(duration),
    };

    data.shrink_to_fit();

    Ok((
        head,
        KeptResponseBody {
            data,
            size,
            truncated,
        },
    ))
}

/// Appends `chunk` to `kept`, the start of a body, up to [`MAX_KEPT_BODY_BYTES`] in total.
fn keep_start(kept: &mut Vec<u8>, chunk: &[u8]) {
    let room = MAX_KEPT_BODY_BYTES - kept.len();
    kept.extend(chunk.iter().take(room));
}

fn captured_body(data: &[u8], size: usize, truncated: bool) -> CapturedBody {
    CapturedBody {
        text: String::from_utf8_lossy(data).into_owned(),
        size: u64::try_from(size).unwrap_or(u64::MAX),
        truncated,
    }
}

/// Lists `headers` as name and value pairs, with the values that usually carry secrets redacted.
fn header_pairs(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| {
            let value = if is_sensitive_name(name.as_str()) {
                REDACTED.to_owned()
            } else {
                String::from_utf8_lossy(value.as_bytes()).into_owned()
            };

            (name.as_str().to_owned(), value)
        })
        .collect()
}

/// Returns the path and query of `uri`, with the values of the query parameters that usually
/// carry secrets redacted.
fn redacted_path(uri: &Uri) -> String {
    let Some(query) = uri.query() else {
        return uri.path().to_owned();
    };

    let query = query
        .split('&')
        .map(|parameter| match parameter.split_once('=') {
            Some((name, _)) if is_sensitive_query_name(name) => format!("{name}={REDACTED}"),
            _ => parameter.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("&");

    format!("{}?{query}", uri.path())
}

/// Returns whether the query parameter `name`, as it is written in a URI, usually carries a
/// secret.
fn is_sensitive_query_name(name: &str) -> bool {
    // A client can write any character of the name percent-encoded.
    form_urlencoded::parse(name.as_bytes())
        .next()
        .is_some_and(|(name, _)| is_sensitive_name(&name))
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn unix_millis(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, millis)
}

#[cfg(test)]
mod tests {
    use std::{
        net::SocketAddr,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use rstest::rstest;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };

    use super::*;

    const PORT: Port = 80;

    fn request(
        version: Version,
        headers: &[(&'static str, &str)],
        frames: Vec<InternalHttpBodyFrame>,
    ) -> HttpRequest<StreamingBody> {
        let mut header_map = HeaderMap::new();
        for (name, value) in headers {
            header_map.insert(*name, HeaderValue::from_str(value).unwrap());
        }

        HttpRequest {
            internal_request: InternalHttpRequest {
                method: Method::GET,
                uri: "/items?id=1".parse().unwrap(),
                headers: header_map,
                version,
                body: StreamingBody::from(frames),
            },
            connection_id: 1,
            request_id: 1,
            port: PORT,
        }
    }

    fn data(len: usize) -> InternalHttpBodyFrame {
        InternalHttpBodyFrame::Data(Payload(Bytes::from(vec![b'x'; len])))
    }

    fn capture(
        store: &ReplayStore,
        request: &HttpRequest<StreamingBody>,
        body_is_complete: bool,
    ) -> Option<CaptureHandle> {
        store.capture(
            request,
            body_is_complete,
            true,
            &ClientStore::new_with_timeout(Duration::from_secs(10), None),
            &IncomingTrafficTransportType::Tcp,
        )
    }

    /// Starts a server that hands each connection to `serve`, and returns its address and the
    /// number of connections that it accepted.
    async fn server<F, Fut>(serve: F) -> (SocketAddr, Arc<AtomicUsize>)
    where
        F: Fn(TcpStream) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let accepted = Arc::new(AtomicUsize::new(0));

        let counter = accepted.clone();
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                counter.fetch_add(1, Ordering::SeqCst);
                serve(stream).await;
            }
        });

        (address, accepted)
    }

    /// Reads the head of an HTTP/1 request from `stream`.
    async fn read_head(stream: &mut TcpStream) {
        let mut head = Vec::new();
        let mut byte = [0; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if stream.read(&mut byte).await.unwrap() == 0 {
                return;
            }
            head.push(byte[0]);
        }
    }

    /// Returns why the first captured request can't be replayed.
    fn first_reason(store: &ReplayStore) -> Option<NotReplayable> {
        store
            .list()
            .first()
            .expect("a request is captured")
            .not_replayable
    }

    fn listen_on(store: &ReplayStore, address: SocketAddr) {
        store.set_listeners([(PORT, &ListeningOn::Socket(address))].into_iter());
    }

    #[rstest]
    #[case::stated_size_without_end(Version::HTTP_11, &[("content-length", "5")], vec![data(5)], None)]
    #[case::stated_size_cut_short(Version::HTTP_11, &[("content-length", "10")], vec![data(5)], Some(NotReplayable::BodyIncomplete))]
    #[case::chunked_without_end(Version::HTTP_11, &[("transfer-encoding", "chunked")], vec![data(5)], Some(NotReplayable::BodyIncomplete))]
    #[case::http2_without_end(Version::HTTP_2, &[("content-length", "5")], vec![data(5)], Some(NotReplayable::BodyIncomplete))]
    #[case::trailers_without_end(Version::HTTP_11, &[("transfer-encoding", "chunked")], vec![data(5), InternalHttpBodyFrame::Trailers(HeaderMap::new())], None)]
    #[tokio::test]
    async fn body_without_its_end_message(
        #[case] version: Version,
        #[case] headers: &[(&'static str, &str)],
        #[case] later_frames: Vec<InternalHttpBodyFrame>,
        #[case] expected: Option<NotReplayable>,
    ) {
        let store = ReplayStore::default();
        let handle = capture(&store, &request(version, headers, vec![]), false).unwrap();

        handle.body_arrived(&later_frames, false);

        assert_eq!(first_reason(&store), expected);
    }

    #[rstest]
    #[case::largest_kept(MAX_KEPT_BODY_BYTES, None)]
    #[case::too_large(MAX_KEPT_BODY_BYTES + 1, Some(NotReplayable::BodyTooLarge))]
    #[tokio::test]
    async fn body_size_limit(#[case] len: usize, #[case] expected: Option<NotReplayable>) {
        let store = ReplayStore::default();
        let headers = [("content-length", len.to_string())];
        let headers = headers
            .each_ref()
            .map(|(name, value)| (*name, value.as_str()));
        capture(
            &store,
            &request(Version::HTTP_11, &headers, vec![data(len)]),
            true,
        );

        assert_eq!(first_reason(&store), expected);
    }

    #[tokio::test]
    async fn upgraded_request_is_not_replayable() {
        let store = ReplayStore::default();
        let handle = capture(&store, &request(Version::HTTP_11, &[], vec![]), true).unwrap();

        handle.responded(
            StatusCode::SWITCHING_PROTOCOLS,
            &HeaderMap::new(),
            Duration::ZERO,
        );

        assert_eq!(first_reason(&store), Some(NotReplayable::Upgraded));
    }

    #[tokio::test]
    async fn health_probe_is_not_captured() {
        let store = ReplayStore::default();
        let probe = request(
            Version::HTTP_11,
            &[("user-agent", "kube-probe/1.30")],
            vec![],
        );

        assert!(capture(&store, &probe, true).is_none());
        assert!(store.list().is_empty());
    }

    #[rstest]
    #[case::no_query("/items", "/items")]
    #[case::secret("/items?token=abc&id=1", "/items?token=[REDACTED]&id=1")]
    #[case::encoded_secret("/items?%74oken=abc", "/items?%74oken=[REDACTED]")]
    #[case::not_secret("/items?id=1", "/items?id=1")]
    fn redacts_secrets_in_path(#[case] uri: &str, #[case] expected: &str) {
        assert_eq!(redacted_path(&uri.parse().unwrap()), expected);
    }

    #[test]
    fn redacts_secret_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer abc"));
        headers.insert("x-request-id", HeaderValue::from_static("7f1c"));

        assert_eq!(
            header_pairs(&headers),
            [
                ("authorization".to_owned(), REDACTED.to_owned()),
                ("x-request-id".to_owned(), "7f1c".to_owned()),
            ]
        );
    }

    #[test]
    fn options_are_read_without_a_content_type() {
        let options = replay_options(br#"{"headers":{"authorization":null}}"#).unwrap();
        assert_eq!(options.headers.get("authorization"), Some(&None));

        assert!(replay_options(b"").unwrap().headers.is_empty());
        assert!(matches!(
            replay_options(b"{"),
            Err(ReplayError::InvalidOptions(..))
        ));
    }

    #[test]
    fn unknown_reason_keeps_the_list_readable() {
        let list =
            r#"[{"id":1,"not_replayable":null},{"id":2,"not_replayable":"body_compressed"}]"#;

        #[derive(serde::Deserialize)]
        struct Summary {
            not_replayable: Option<NotReplayable>,
        }
        let list: Vec<Summary> = serde_json::from_str(list).unwrap();

        let reasons: Vec<_> = list.iter().map(|summary| summary.not_replayable).collect();
        assert_eq!(reasons, [None, Some(NotReplayable::Unknown)]);
    }

    /// The local application can have handled a request whose connection it dropped, so the
    /// replay must not send it again.
    #[tokio::test]
    async fn replay_is_not_retried_after_the_request_was_read() {
        let (address, accepted) = server(|mut stream| async move {
            read_head(&mut stream).await;
        })
        .await;
        let store = ReplayStore::default();
        capture(&store, &request(Version::HTTP_11, &[], vec![]), true);
        listen_on(&store, address);

        let result = store.replay(1, ReplayOptions::default()).await;

        assert!(matches!(result, Err(ReplayError::Send(..))), "{result:?}");
        assert_eq!(accepted.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn replay_reaches_the_current_address() {
        let ok = |mut stream: TcpStream| async move {
            read_head(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
                .await
                .unwrap();
        };
        let (old_address, old_accepted) = server(ok).await;
        let (new_address, new_accepted) = server(ok).await;
        let store = ReplayStore::default();
        listen_on(&store, old_address);
        capture(&store, &request(Version::HTTP_11, &[], vec![]), true);

        listen_on(&store, new_address);
        let run = store.replay(1, ReplayOptions::default()).await.unwrap();

        assert_eq!(run.body.text, "ok");
        assert_eq!(old_accepted.load(Ordering::SeqCst), 0);
        assert_eq!(new_accepted.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failed_replay_says_why_the_connection_failed() {
        let (address, _) = server(|mut stream| async move {
            read_head(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 1000\r\n\r\npartial")
                .await
                .unwrap();
            // Usually lets the replay read the response head first, but the test passes either way.
            tokio::time::sleep(Duration::from_millis(200)).await;
            // Closing with a zero linger time resets the connection.
            socket2::SockRef::from(&stream)
                .set_linger(Some(Duration::ZERO))
                .unwrap();
        })
        .await;
        let store = ReplayStore::default();
        capture(&store, &request(Version::HTTP_11, &[], vec![]), true);
        listen_on(&store, address);

        let error = store.replay(1, ReplayOptions::default()).await.unwrap_err();

        assert!(matches!(error, ReplayError::Send(..)), "{error:?}");
        let mut cause: &dyn std::error::Error = &error;
        while let Some(source) = cause.source() {
            cause = source;
        }
        let reset = cause
            .downcast_ref::<std::io::Error>()
            .expect("the root cause is the reset of the connection");
        assert_eq!(reset.kind(), std::io::ErrorKind::ConnectionReset);
        assert!(error.to_string().ends_with(&reset.to_string()), "{error}");
    }

    /// A replay that finishes after newer requests pushed its request out gets a number of its
    /// own, not the number of a replay that finished meanwhile.
    #[tokio::test]
    async fn replay_finishing_after_eviction_gets_its_own_number() {
        let (address, accepted) = server(|mut stream| async move {
            read_head(&mut stream).await;
            tokio::spawn(async move {
                // The first connection answers late, the others at once.
                if FIRST.swap(false, Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
                    .await
                    .unwrap();
            });
        })
        .await;
        static FIRST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
        let store = ReplayStore::default();
        capture(&store, &request(Version::HTTP_11, &[], vec![]), true);
        listen_on(&store, address);

        let slow = tokio::spawn({
            let store = store.clone();
            async move { store.replay(1, ReplayOptions::default()).await }
        });
        while accepted.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let fast = store.replay(1, ReplayOptions::default()).await.unwrap();
        for _ in 0..MAX_KEPT_REQUESTS {
            capture(&store, &request(Version::HTTP_11, &[], vec![]), true);
        }
        let slow = slow.await.unwrap().unwrap();

        assert_eq!(fast.number, 1);
        assert_eq!(slow.number, 2);
    }

    /// A response that never ends, such as an event stream, is cut after
    /// [`RESPONSE_BODY_IDLE_TIMEOUT`], and its connection isn't used again.
    #[tokio::test]
    async fn endless_response_is_truncated() {
        let (address, accepted) = server(|mut stream| async move {
            read_head(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n5\r\nhello\r\n")
                .await
                .unwrap();
            tokio::spawn(async move {
                let _stream = stream;
                std::future::pending::<()>().await;
            });
        })
        .await;
        let store = ReplayStore::default();
        capture(&store, &request(Version::HTTP_11, &[], vec![]), true);
        listen_on(&store, address);

        let started = Instant::now();
        let run = store.replay(1, ReplayOptions::default()).await.unwrap();
        assert!(
            started.elapsed() < RESPONSE_BODY_TIMEOUT,
            "cut by the idle timeout"
        );
        assert!(run.body.truncated);
        assert_eq!(run.body.text, "hello");

        store.replay(1, ReplayOptions::default()).await.unwrap();
        assert_eq!(accepted.load(Ordering::SeqCst), 2);
    }
}
