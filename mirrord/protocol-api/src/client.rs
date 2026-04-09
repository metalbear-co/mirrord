use std::{
    net::SocketAddr,
    num::NonZeroUsize,
    ops::Not,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::{FutureExt, future::Shared};
use mirrord_protocol::{
    LogMessage,
    outgoing::UnixAddr,
    tcp::{
        HTTP_BODY_JSON_FILTER_VERSION, HTTP_COMPOSITE_FILTER_VERSION,
        HTTP_HEADER_JQ_FILTER_VERSION, HTTP_METHOD_FILTER_VERSION, HttpBodyFilter, HttpFilter,
        MIRROR_HTTP_FILTER_VERSION,
    },
};
use tokio::{
    sync::{mpsc, oneshot},
    task::{AbortHandle, JoinHandle},
};
use tokio_stream::wrappers::ReceiverStream;

pub use crate::client::{
    config::ClientConfig,
    connector::{Connection, ProtocolConnector},
    error::ClientError,
    request::ClientRequest,
};
use crate::{
    client::{
        error::{ClientResult, TaskError, TaskResult},
        incoming::IncomingMode,
        outgoing::OutgoingMode,
        request::ClientRequestStream,
        task::ClientTask,
    },
    fifo::{Fifo, FifoStream},
    traffic::{TunneledIncoming, TunneledOutgoing},
};

mod config;
mod connector;
mod enum_map;
mod error;
mod incoming;
mod outbox;
mod outgoing;
mod ping_pong;
mod queue_kind;
mod request;
mod simple_request;
mod task;
#[cfg(test)]
mod test;
mod tunnels;

/// Public client API of a [`mirrord_protocol`] session, possibly spanning multiple connections.
///
/// All instances acquired with [`Self::clone_with_new_queue`] and [`Self::clone_with_same_queue`]
/// are connected to the same background task. The background task exits when all clients are
/// dropped.
///
/// # Protocol compatibility
///
/// This client ensures that the server can handle all produced [`mirrord_protocol`] messages. If
/// you attempt to send a message that cannot be handled by the server, one of two things will
/// happen:
/// 1. You'll get [`ClientError::NotSupported`]; OR
/// 2. This client will transparently downgrade the message (if possible).
///
/// To expose a sane API, this client does not handle [`mirrord_protocol`] version
/// downgrades. When the first connection is established, the protocol version of that connection
/// will be used as a required version for all later connections. This behavior greatly
/// simplifies the logic and allows for exposing a simple API.
///
/// # Reconnects and retries
///
/// As long as the [`ProtocolConnector`] allows it, this client will attempt
/// reconnecting to the server after connection failures.
///
/// After a reconnection, this client remains valid and functional.
/// The background task does not consume requests while reconnecting,
/// so requests can be retried with a "busy" loop.
///
/// # Fairness
///
/// Since a [`mirrord_protocol`] connection is just a single stream of messages,
/// concurrent requests and traffic streams naturally compete for its capacity.
/// This client attempts to mitigate the problem by putting all outbound [`mirrord_protocol`]
/// messages in a set of queues and processing all queues in a round-robin fashion.
///
/// The set of outbound queues contains:
/// 1. Queues for requests from [`MirrordClient`] instances
/// 2. Queues for established traffic tunnels (raw connections and HTTP requests)
///
/// # Cloning
///
/// This client does not implement [`Clone`]. Instead, you have explicit methods to get a new
/// instance: [`Self::clone_with_new_queue`] and [`Self::clone_with_same_queue`].
#[derive(Debug)]
pub struct MirrordClient {
    protocol_version: semver::Version,
    request_tx: mpsc::Sender<ClientRequest>,
    new_client_tx: mpsc::Sender<ClientRequestStream>,
    task_fut: Shared<TaskErrorFut>,
    task_abort: AbortHandle,
    logs_fifo_capacity: NonZeroUsize,
}

impl MirrordClient {
    pub async fn new<C: ProtocolConnector>(
        connector: C,
        config: ClientConfig,
        channel_size: NonZeroUsize,
    ) -> Result<Self, TaskError> {
        let logs_fifo_capacity = config.logs_fifo_capacity;
        let (new_client_tx, new_client_rx) = mpsc::channel(8);
        let task = ClientTask::new(connector, new_client_rx, config).await?;
        let protocol_version = task.protocol_version().clone();
        let task = tokio::spawn(task.run());
        let task_abort = task.abort_handle();
        let task_fut = TaskErrorFut(task).shared();

        let (request_tx, request_rx) = mpsc::channel(channel_size.get());
        new_client_tx.try_send(ReceiverStream::new(request_rx)).ok();

        let client = Self {
            protocol_version,
            request_tx,
            new_client_tx,
            task_fut,
            task_abort,
            logs_fifo_capacity,
        };
        Ok(client)
    }

    /// Gets a new instance, which has its independent outbound queue.
    ///
    /// See the struct doc on fairness for more info.
    pub async fn clone_with_new_queue(&self, channel_size: NonZeroUsize) -> Self {
        let (request_tx, request_rx) = mpsc::channel(channel_size.get());
        self.new_client_tx
            .send(ReceiverStream::new(request_rx))
            .await
            .ok();
        Self {
            protocol_version: self.protocol_version.clone(),
            request_tx,
            new_client_tx: self.new_client_tx.clone(),
            task_fut: self.task_fut.clone(),
            task_abort: self.task_abort.clone(),
            logs_fifo_capacity: self.logs_fifo_capacity,
        }
    }

    /// Gets a new instance, which shares the same outbound queue.
    ///
    /// See the struct doc on fairness for more info.
    pub fn clone_with_same_queue(&self) -> Self {
        Self {
            protocol_version: self.protocol_version.clone(),
            request_tx: self.request_tx.clone(),
            new_client_tx: self.new_client_tx.clone(),
            task_fut: self.task_fut.clone(),
            task_abort: self.task_abort.clone(),
            logs_fifo_capacity: self.logs_fifo_capacity,
        }
    }

    /// Returns the size of this instance's queue.
    pub fn queue_size(&self) -> NonZeroUsize {
        NonZeroUsize::new(self.request_tx.capacity()).expect("cannot be zero")
    }

    /// Returns whether two instances use the request queue.
    pub fn same_queue(&self, other: &Self) -> bool {
        self.request_tx.same_channel(&other.request_tx)
    }

    /// Returns whether two instances use the same background task.
    pub fn same_task(&self, other: &Self) -> bool {
        self.task_fut.ptr_eq(&other.task_fut)
    }

    /// Returns whether this client is failed.
    ///
    /// Failure means that there is no working connection to the server,
    /// and no more reconnection attempts will be made.
    pub fn is_failed(&self) -> bool {
        self.request_tx.is_closed()
    }

    /// Waits for this client to fail.
    ///
    /// Failure means that there is no working connection to the server,
    /// and no more reconnection attempts will be made.
    pub async fn failed(&self) -> TaskError {
        self.task_fut.clone().await
    }

    /// Returns the version of the [`mirrord_protocol`] used.
    ///
    /// This *can* be lower than [`mirrord_protocol::VERSION`], since the version must be supported
    /// by both the client and the server.
    pub fn protocol_version(&self) -> &semver::Version {
        &self.protocol_version
    }

    /// Aborts the background task that runs this client.
    pub fn abort_task(&self) {
        self.task_abort.abort();
    }

    /// Makes an outgoing TCP/UDP connection (purely logical in the case of UDP).
    ///
    /// Note that [`Fifo`]s used in the tunnel are subject to the limits set in the
    /// [`ClientConfig`]. If you don't consume the traffic in time, it will be dropped (remote peer
    /// will be notified).
    pub async fn connect_ip(
        &self,
        addr: SocketAddr,
        mode: OutgoingMode,
    ) -> ClientResult<TunneledOutgoing<SocketAddr>> {
        let (tx, rx) = oneshot::channel();
        self.request_tx
            .send(ClientRequest::ConnectIp {
                addr,
                mode,
                response_tx: tx,
            })
            .await
            .ok();
        match rx.await {
            Ok(result) => result,
            Err(..) => Err(ClientError::TaskFailed(self.failed().await)),
        }
    }

    /// Makes an outgoing UNIX connection.
    ///
    /// Note that [`Fifo`]s used in the tunnel are subject to the limits set in the
    /// [`ClientConfig`]. If you don't consume the traffic in time, it will be dropped (remote peer
    /// will be notified).
    pub async fn connect_unix(&self, addr: UnixAddr) -> ClientResult<TunneledOutgoing<UnixAddr>> {
        let (tx, rx) = oneshot::channel();
        self.request_tx
            .send(ClientRequest::ConnectUnix {
                addr,
                response_tx: tx,
            })
            .await
            .ok();
        match rx.await {
            Ok(result) => result,
            Err(..) => Err(ClientError::TaskFailed(self.failed().await)),
        }
    }

    /// Subscribes to incoming traffic on the given port.
    ///
    /// Subsequent calls with the same (port + steal/mirror) combo overwrite the previous
    /// subscription. New traffic is redirected to the new stream, and the previous stream is
    /// closed.
    ///
    /// Note that `filter` might be ignored for mirroring subscriptions if the server's
    /// [`mirrord_protocol`] version does not support it.
    ///
    /// # Return
    ///
    /// Returns a [`FifoStream`] that will receive incoming traffic.
    /// Dropping the stream will unsubscribe from the port.
    ///
    /// If the stream is closed, it means that this client lost the connection,
    /// or the subscription was overwritten.
    ///
    /// Note that the [`Fifo`] is subject to the limits set in the [`ClientConfig`].
    /// If you don't consume the traffic in time, it will be dropped (remote peer will be notified).
    pub async fn subscribe_port(
        &self,
        port: u16,
        mode: IncomingMode,
        mut filter: Option<HttpFilter>,
    ) -> ClientResult<FifoStream<TunneledIncoming>> {
        fn filter_supported(filter: &HttpFilter, version: &semver::Version) -> bool {
            match filter {
                HttpFilter::Path(..) | HttpFilter::Header(..) => true,
                HttpFilter::Body(HttpBodyFilter::Json { .. }) => {
                    HTTP_BODY_JSON_FILTER_VERSION.matches(version)
                }
                HttpFilter::Method(..) => HTTP_METHOD_FILTER_VERSION.matches(version),
                HttpFilter::HeaderJq(..) => HTTP_HEADER_JQ_FILTER_VERSION.matches(version),
                HttpFilter::Composite { filters, .. } => {
                    HTTP_COMPOSITE_FILTER_VERSION.matches(version)
                        && filters.iter().all(|f| filter_supported(f, version))
                }
            }
        }

        if let Some(used_filter) = &filter {
            match mode {
                IncomingMode::Mirror
                    if MIRROR_HTTP_FILTER_VERSION
                        .matches(&self.protocol_version)
                        .not() =>
                {
                    filter = None;
                }
                _ if filter_supported(used_filter, &self.protocol_version).not() => {
                    return Err(ClientError::NotSupported);
                }
                _ => {}
            }
        }

        let (tx, rx) = oneshot::channel();
        self.request_tx
            .send(ClientRequest::SubscribePort {
                port,
                mode,
                filter,
                response_tx: tx,
            })
            .await
            .ok();
        match rx.await {
            Ok(result) => result,
            Err(..) => Err(ClientError::TaskFailed(self.failed().await)),
        }
    }

    /// Subscribes to server logs.
    ///
    /// Subsequent calls overwrite previous subscription.
    /// New logs are redirected to the new stream, and the previous stream is closed.
    ///
    /// # Return
    ///
    /// Returns a [`FifoStream`] that will receive logs.
    /// Dropping the stream will unsubscribe from logs (new logs will be discarded).
    ///
    /// If the stream is closed, it means that this client has lost the connection, or
    /// the subscription was overwritten.
    ///
    /// Note that the [`Fifo`] is subject to the limits set in the [`ClientConfig`].
    /// If you don't consume the logs in time, they will be dropped.
    pub async fn subscribe_logs(&self) -> ClientResult<FifoStream<LogMessage>> {
        let fifo = Fifo::with_capacity(self.logs_fifo_capacity);
        if self
            .request_tx
            .send(ClientRequest::Logs(fifo.sink))
            .await
            .is_err()
        {
            Err(ClientError::TaskFailed(self.failed().await))
        } else {
            Ok(fifo.stream)
        }
    }

    /// Makes a [`SimpleRequest`].
    ///
    /// Note that the request can be transparently downgraded to fit the server's
    /// [`mirrord_protocol`] version.
    pub async fn make_request<R: SimpleRequest>(&self, request: R) -> ClientResult<R::Response> {
        let prepared = request.prepare(&self.protocol_version)?;
        self.request_tx
            .send(ClientRequest::Simple {
                message: prepared.request,
                queue_kind: prepared.queue_kind,
                handler: prepared.result_handler,
            })
            .await
            .ok();
        match prepared.result_rx.await {
            Ok(result) => result,
            Err(..) => Err(ClientError::TaskFailed(self.failed().await)),
        }
    }

    /// Makes a [`SimpleRequestNoResponse`].
    ///
    /// As these requests do not trigger any response, this function swallows errors for the sake of
    /// simplicity.
    pub async fn make_request_no_response<R: SimpleRequestNoResponse>(&self, request: R) {
        let prepared = request.prepare();
        self.request_tx
            .send(ClientRequest::SimpleNoResponse(prepared))
            .await
            .ok();
    }
}

/// Trait guard - prevents implementing the sealed trait outside of this crate.
pub trait SimpleRequest: simple_request::SimpleRequest {}

impl<T: simple_request::SimpleRequest> SimpleRequest for T {}

/// Trait guard - prevents implementing the sealed trait outside of this crate.
pub trait SimpleRequestNoResponse: simple_request::SimpleRequestNoResponse {}

impl<T: simple_request::SimpleRequestNoResponse> SimpleRequestNoResponse for T {}

/// [`Future`] that maps the output of a [`ClientTask`]'s [`JoinHandle`] into a [`TaskError`].
///
/// As [`TaskError`] is cloneable, this allows for wrapping the [`JoinHandle`] into [`Shared`].
struct TaskErrorFut(JoinHandle<TaskResult<()>>);

impl Future for TaskErrorFut {
    type Output = TaskError;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().0.poll_unpin(cx).map(|result| {
            result
                .map_err(Arc::new)
                .map_err(TaskError::JoinError)
                .unwrap_or_else(Err)
                .err()
                .unwrap_or(TaskError::UnexpectedlyFinished)
        })
    }
}
