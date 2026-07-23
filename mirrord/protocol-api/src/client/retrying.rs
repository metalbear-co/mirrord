use std::{net::SocketAddr, ops::Not, time::Duration};

use futures::{StreamExt, TryStreamExt, stream::BoxStream};
use mirrord_protocol::{LogMessage, outgoing::UnixAddr, tcp::HttpFilter};
use tokio_retry::strategy::ExponentialBackoff;

use crate::{
    client::{
        ClientError, MirrordClient, SimpleRequest, error::ClientResult, incoming::IncomingMode,
        outgoing::OutgoingMode,
    },
    traffic::{TunneledIncoming, TunneledOutgoing},
};

/// Extension trait for [`MirrordClient`].
///
/// Provides methods for making [`mirrord_protocol`] requests with automatic retries upon
/// reconnects.
///
/// The request is retried when it fails with:
/// 1. [`ClientError::ConnectionLost`] - retried immediately. This is safe (does not introduce any
///    busy loop), because the background task does not process client requests when there is no
///    [`mirrord_protocol`] server connection.
/// 2. A retryable [`ClientError::Response`] (a `can_retry` agent error, e.g. the agent was lost and
///    is being respun) - retried after an exponentially growing backoff, for a bounded number of
///    attempts, since unlike a lost connection these errors return immediately and would otherwise
///    busy-loop.
///
/// All other errors are considered fatal.
///
/// # Timeouts
///
/// All methods of this trait require specifying a timeout. The timeout policy is as follows:
/// 1. If the timeout elapses before the first request attempt completes, the request is failed with
///    [`ClientError::Timeout`].
/// 2. If the timeout elapses after some request attempts fail, the request is failed with the error
///    from the most recent attempt.
///
/// # Retrying streams
///
/// [`MirrordClientRetry::subscribe_port_retry`] and [`MirrordClientRetry::subscribe_logs_retry`]
/// return [`Stream`](futures::stream::Stream)s that transparently handle resubscribing after
/// reconnects. The streams have to be polled in order to progress (re)subscription requests.
///
/// Every (re)subscription is subject to the timeout policy explained above.
///
/// The streams do not yield errors after retryable failures.
/// If a stream yields an error, that error is fatal and the stream is dead.
pub trait MirrordClientRetry {
    /// Makes a retrying [`MirrordClient::connect_ip`] request.
    ///
    /// See this trait's doc for more context.
    fn connect_ip_retry(
        &self,
        addr: SocketAddr,
        mode: OutgoingMode,
        timeout: Duration,
    ) -> impl Future<Output = ClientResult<TunneledOutgoing<SocketAddr>>> + Send;

    /// Makes a retrying [`MirrordClient::connect_unix`] request.
    ///
    /// See this trait's doc for more context.
    fn connect_unix_retry(
        &self,
        addr: UnixAddr,
        timeout: Duration,
    ) -> impl Future<Output = ClientResult<TunneledOutgoing<UnixAddr>>> + Send;

    /// Makes a retrying [`SimpleRequest`].
    ///
    /// See this trait's doc for more context.
    fn make_request_retry<R: SimpleRequest + Clone + Sync>(
        &self,
        request: R,
        timeout: Duration,
    ) -> impl Future<Output = ClientResult<R::Response>> + Send;

    /// Creates a retrying [`MirrordClient::subscribe_port`] stream.
    ///
    /// See this trait's doc for more context.
    fn subscribe_port_retry(
        self,
        port: u16,
        mode: IncomingMode,
        filter: Option<HttpFilter>,
        timeout: Duration,
    ) -> BoxStream<'static, ClientResult<TunneledIncoming>>;

    /// Creates a retrying [`MirrordClient::subscribe_logs`] stream.
    ///
    /// See this trait's doc for more context.
    fn subscribe_logs_retry(
        self,
        timeout: Duration,
    ) -> BoxStream<'static, ClientResult<LogMessage>>;
}

impl MirrordClientRetry for MirrordClient {
    async fn connect_ip_retry(
        &self,
        addr: SocketAddr,
        mode: OutgoingMode,
        timeout: Duration,
    ) -> ClientResult<TunneledOutgoing<SocketAddr>> {
        retry_op(|| self.connect_ip(addr, mode), timeout).await
    }

    async fn connect_unix_retry(
        &self,
        addr: UnixAddr,
        timeout: Duration,
    ) -> ClientResult<TunneledOutgoing<UnixAddr>> {
        retry_op(|| self.connect_unix(addr.clone()), timeout).await
    }

    async fn make_request_retry<R: SimpleRequest + Clone + Sync>(
        &self,
        request: R,
        timeout: Duration,
    ) -> ClientResult<R::Response> {
        if request.is_stateless() {
            retry_op(|| self.make_request(request.clone()), timeout).await
        } else {
            // If the request is stateful, we know that we can't retry.
            // In order to avoid cloning the request, we make just one attempt
            // (the request might be large, for example `WriteFileRequest`).
            tokio::time::timeout(timeout, self.make_request(request))
                .await
                .unwrap_or_else(|_| Err(ClientError::Timeout(timeout)))
        }
    }

    fn subscribe_port_retry(
        self,
        port: u16,
        mode: IncomingMode,
        filter: Option<HttpFilter>,
        timeout: Duration,
    ) -> BoxStream<'static, ClientResult<TunneledIncoming>> {
        let start_subscription = move |(client, filter): (MirrordClient, Option<HttpFilter>)| async move {
            let fifo = retry_op(
                || client.subscribe_port(port, mode, filter.clone()),
                timeout,
            )
            .await?;
            Ok::<_, ClientError>(Some((fifo, (client, filter))))
        };

        let mut had_error = false;

        futures::stream::try_unfold((self, filter), start_subscription)
            .map_ok(|fifo| fifo.map(Ok))
            .try_flatten()
            .take_while(move |item| match item {
                Ok(..) => std::future::ready(true),
                Err(..) => {
                    // Finish *after* the first error.
                    let take = had_error.not();
                    had_error = true;
                    std::future::ready(take)
                }
            })
            .boxed()
    }

    fn subscribe_logs_retry(
        self,
        timeout: Duration,
    ) -> BoxStream<'static, ClientResult<LogMessage>> {
        let start_subscription = move |client: MirrordClient| async move {
            let fifo = retry_op(|| client.subscribe_logs(), timeout).await?;
            Ok::<_, ClientError>(Some((fifo, client)))
        };

        let mut had_error = false;

        futures::stream::try_unfold(self, start_subscription)
            .map_ok(|fifo| fifo.map(Ok))
            .try_flatten()
            .take_while(move |item| match item {
                Ok(..) => std::future::ready(true),
                Err(..) => {
                    // Finish *after* the first error.
                    let take = had_error.not();
                    had_error = true;
                    std::future::ready(take)
                }
            })
            .boxed()
    }
}

/// Maximum number of times a retryable [`ClientError::Response`] (a `can_retry` agent error) is
/// retried before giving up, each after an exponentially growing backoff.
const MAX_RETRYABLE_ERROR_ATTEMPTS: usize = 8;

/// Retries the given async operation according to the [`MirrordClientRetry`] policies.
async fn retry_op<F, Fut, R>(mut make_fut: F, timeout: Duration) -> ClientResult<R>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = ClientResult<R>>,
{
    let mut timeout_fut = std::pin::pin!(tokio::time::sleep(timeout));
    let mut retryable_error_backoff = ExponentialBackoff::from_millis(2)
        .factor(125)
        .max_delay(Duration::from_secs(2))
        .take(MAX_RETRYABLE_ERROR_ATTEMPTS);
    let mut last_error = None;
    loop {
        let backoff = tokio::select! {
            result = make_fut() => match result {
                Ok(value) => break Ok(value),
                Err(error @ ClientError::ConnectionLost(..)) => {
                    last_error = Some(error);
                    None
                }
                Err(error) if matches!(&error, ClientError::Response(e) if e.can_retry()) => {
                    match retryable_error_backoff.next() {
                        Some(delay) => {
                            last_error = Some(error);
                            Some(delay)
                        }
                        None => break Err(error),
                    }
                }
                Err(error) => break Err(error),
            },
            _ = &mut timeout_fut => {
                break Err(last_error.unwrap_or(ClientError::Timeout(timeout)));
            },
        };

        if let Some(delay) = backoff {
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = &mut timeout_fut => {
                    break Err(last_error.unwrap_or(ClientError::Timeout(timeout)));
                }
            }
        }
    }
}
