use std::{
    fmt,
    net::SocketAddr,
    ops::Deref,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{Router, extract::State, routing::get};
use bincode::Encode;
use http::StatusCode;
use mirrord_protocol::{ClientMessage, DaemonMessage, Measured, MessageKind};
use prometheus::{GaugeVec, IntGauge, IntGaugeVec, Registry, proto::MetricFamily};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::Level;

use crate::error::AgentError;

/// Incremented whenever we get a new client in `ClientConnectionHandler`, and decremented
/// when this client is dropped.
pub(crate) static CLIENT_COUNT: AtomicUsize = AtomicUsize::new(0);

/// How many DNS resolution client requests the agent is currently handling.
pub(crate) static DNS_REQUEST_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Incremented and decremented in _open-ish_/_close-ish_ file operations in `FileManager`,
/// Also gets decremented when `FileManager` is dropped.
pub(crate) static OPEN_FD_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Follows the amount of subscribed ports in `update_packet_filter`. We don't really
/// increment/decrement this one, and mostly `set` it to the latest amount of ports, zeroing it when
/// the `TcpConnectionSniffer` gets dropped.
pub(crate) static MIRROR_PORT_SUBSCRIPTION: AtomicUsize = AtomicUsize::new(0);

pub(crate) static MIRROR_CONNECTION_SUBSCRIPTION: AtomicUsize = AtomicUsize::new(0);

pub(crate) static STEAL_FILTERED_PORT_SUBSCRIPTION: AtomicUsize = AtomicUsize::new(0);

pub(crate) static STEAL_UNFILTERED_PORT_SUBSCRIPTION: AtomicUsize = AtomicUsize::new(0);

pub(crate) static REDIRECTED_CONNECTIONS: AtomicUsize = AtomicUsize::new(0);

pub(crate) static REDIRECTED_REQUESTS: AtomicUsize = AtomicUsize::new(0);

pub(crate) static TCP_OUTGOING_CONNECTION: AtomicUsize = AtomicUsize::new(0);

pub(crate) static UDP_OUTGOING_CONNECTION: AtomicUsize = AtomicUsize::new(0);

pub(crate) static SEQPACKET_CONNECTION: AtomicUsize = AtomicUsize::new(0);

/// Metrics for tracking bypassed requests (a request that did not match an http filter or wasn't
/// stolen by the stealer task).
///
/// - Also consumed by the operator.
pub(crate) static BYPASSED_REQUESTS: LazyLock<prometheus::GaugeVec> = LazyLock::new(|| {
    prometheus::register_gauge_vec!(
        "mirrord_agent_bypassed_requests",
        "amount of incoming requests currently bypassed by mirrord-agent",
        &["port"]
    )
    .expect("BYPASSED_REQUESTS should be valid")
});

/// Memory held by mirrord-protocol messages. Messages pile up in the agent when a client reads
/// slowly or traffic comes in bursts, and this shows how much of the agent's memory they take.
///
/// Workload-companion connections go through protocol-io, a shared crate whose queues hold
/// encoded bytes and decoded messages rather than [`Buffered`] ones, so what waits in them is not
/// counted: about 16 KiB plus one message on its way to the client, and up to 64 messages from it.
///
/// Updated live by [`Buffered`] messages, through [`BUFFERED_MESSAGE_GAUGES`].
pub(crate) static BUFFERED_MESSAGES_BYTES: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    prometheus::register_int_gauge_vec!(
        "mirrord_agent_buffered_messages_bytes",
        "bincode-encoded size of mirrord-protocol messages held in mirrord-agent memory, while \
         queued, being handled, or being sent; the sum over all labels is the agent's total",
        &["direction", "message"]
    )
    // The name, help and label names are constants, and the `LazyLock` registers once, so this
    // fails only on a programming error: an invalid name or a duplicate registration.
    .expect("BUFFERED_MESSAGES_BYTES should be valid")
});

/// The children of [`BUFFERED_MESSAGES_BYTES`], resolved once, so that counting a message does
/// not look its series up by label values.
static BUFFERED_MESSAGE_GAUGES: LazyLock<BufferedMessageGauges> =
    LazyLock::new(|| BufferedMessageGauges {
        client: MessageGauges::new("client"),
        daemon: MessageGauges::new("daemon"),
    });

/// [`BUFFERED_MESSAGES_BYTES`] children by `direction`: `client` for [`ClientMessage`]s and
/// `daemon` for [`DaemonMessage`]s.
struct BufferedMessageGauges {
    client: MessageGauges,
    daemon: MessageGauges,
}

/// [`BUFFERED_MESSAGES_BYTES`] children of one `direction`, one per [`MessageKind`].
///
/// Named fields rather than an array indexed by the kind, so a new [`MessageKind`] fails to
/// compile in [`Self::get`] until it has a gauge, instead of indexing out of bounds at runtime.
struct MessageGauges {
    tcp: IntGauge,
    tcp_steal: IntGauge,
    tcp_outgoing: IntGauge,
    udp_outgoing: IntGauge,
    seqpacket_outgoing: IntGauge,
    file: IntGauge,
    other: IntGauge,
}

impl MessageGauges {
    fn new(direction: &str) -> Self {
        let gauge = |kind: MessageKind| {
            BUFFERED_MESSAGES_BYTES.with_label_values(&[direction, <&str>::from(kind)])
        };

        Self {
            tcp: gauge(MessageKind::Tcp),
            tcp_steal: gauge(MessageKind::TcpSteal),
            tcp_outgoing: gauge(MessageKind::TcpOutgoing),
            udp_outgoing: gauge(MessageKind::UdpOutgoing),
            seqpacket_outgoing: gauge(MessageKind::SeqpacketOutgoing),
            file: gauge(MessageKind::File),
            other: gauge(MessageKind::Other),
        }
    }

    fn get(&self, kind: MessageKind) -> &IntGauge {
        match kind {
            MessageKind::Tcp => &self.tcp,
            MessageKind::TcpSteal => &self.tcp_steal,
            MessageKind::TcpOutgoing => &self.tcp_outgoing,
            MessageKind::UdpOutgoing => &self.udp_outgoing,
            MessageKind::SeqpacketOutgoing => &self.seqpacket_outgoing,
            MessageKind::File => &self.file,
            MessageKind::Other => &self.other,
        }
    }
}

/// A mirrord-protocol message type whose buffered bytes are reported in
/// [`BUFFERED_MESSAGES_BYTES`].
pub(crate) trait BufferedMessage {
    /// The [`BUFFERED_MESSAGES_BYTES`] child that counts this message.
    fn gauge(&self) -> &'static IntGauge;
}

impl BufferedMessage for ClientMessage {
    fn gauge(&self) -> &'static IntGauge {
        BUFFERED_MESSAGE_GAUGES.client.get(self.kind())
    }
}

impl BufferedMessage for DaemonMessage {
    fn gauge(&self) -> &'static IntGauge {
        BUFFERED_MESSAGE_GAUGES.daemon.get(self.kind())
    }
}

/// Adds an amount to an [`IntGauge`] while alive, and subtracts it on drop.
///
/// Keeps a gauge that reports memory in step with the value that holds the memory, so it cannot
/// drift when that value is dropped on an error path.
#[derive(Debug)]
pub(crate) struct IntGaugeGuard {
    gauge: IntGauge,
    amount: i64,
}

impl IntGaugeGuard {
    /// Adds `amount` to `gauge`, saturating at [`i64::MAX`].
    pub(crate) fn new(gauge: IntGauge, amount: usize) -> Self {
        let amount = amount.try_into().unwrap_or(i64::MAX);
        gauge.add(amount);

        Self { gauge, amount }
    }
}

impl Drop for IntGaugeGuard {
    fn drop(&mut self) {
        self.gauge.sub(self.amount);
    }
}

/// A mirrord-protocol message whose encoded size is counted in [`BUFFERED_MESSAGES_BYTES`] while
/// it exists.
///
/// Holds a [`Measured`] message, so the size counted here is the one the codec reuses when it
/// encodes the message.
pub(crate) struct Buffered<M> {
    message: Measured<M>,
    guard: IntGaugeGuard,
}

impl<M: Encode + BufferedMessage> Buffered<M> {
    pub(crate) fn new(message: M) -> Self {
        Self::from_measured(Measured::new(message))
    }
}

impl<M: BufferedMessage> Buffered<M> {
    /// Counts a message that already has its size, such as one from
    /// [`MeasuredCodec`](mirrord_protocol::MeasuredCodec).
    pub(crate) fn from_measured(message: Measured<M>) -> Self {
        let guard = IntGaugeGuard::new(message.gauge().clone(), message.encoded_len());
        Self { message, guard }
    }
}

impl<M> Buffered<M> {
    /// Splits the message from its guard.
    ///
    /// The message's bytes are still in memory after the message is moved into a codec or a
    /// handler, so the caller should keep the guard until they are gone: sent and flushed, or
    /// handled.
    pub(crate) fn into_parts(self) -> (Measured<M>, IntGaugeGuard) {
        (self.message, self.guard)
    }
}

impl<M> Deref for Buffered<M> {
    type Target = M;

    fn deref(&self) -> &Self::Target {
        &self.message
    }
}

impl<M: fmt::Debug> fmt::Debug for Buffered<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

/// Memory held by the outgoing traffic [`Throttle`](crate::util::io::throttle::Throttle)s, which
/// let each client buffer up to a fixed amount of outgoing traffic data in each direction.
///
/// The value is the throttles' permits in use: each chunk of data costs its length plus a fixed
/// per-chunk overhead. `direction` uses the vocabulary of [`BUFFERED_MESSAGES_BYTES`]: `daemon`
/// for data read from remote peers on its way to the client, `client` for data from the client on
/// its way to remote peers.
///
/// Data on its way to the client stays under its permit while the agent sends the
/// [`DaemonMessage`] that carries it, so for that time it is also counted in
/// [`BUFFERED_MESSAGES_BYTES`] (`direction="daemon"`, `message="tcp_outgoing"`,
/// `"udp_outgoing"` or `"seqpacket_outgoing"`). Adding the two metrics counts those bytes twice.
///
/// Updated live by the throttles' permits, through [`OUTGOING_THROTTLE_PERMITS_DAEMON`] and
/// [`OUTGOING_THROTTLE_PERMITS_CLIENT`].
static OUTGOING_THROTTLE_PERMITS: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    prometheus::register_int_gauge_vec!(
        "mirrord_agent_outgoing_throttle_permits",
        "permits held by the mirrord-agent outgoing traffic throttles (data bytes plus a fixed \
         per-chunk overhead): direction=daemon waits to be sent to the client, direction=client \
         to the remote peer; daemon data is also in mirrord_agent_buffered_messages_bytes while \
         it is being sent, so do not add the two metrics",
        &["direction"]
    )
    // The name, help and label names are constants, and the `LazyLock` registers once, so this
    // fails only on a programming error: an invalid name or a duplicate registration.
    .expect("OUTGOING_THROTTLE_PERMITS should be valid")
});

/// [`OUTGOING_THROTTLE_PERMITS`] child for data read from remote peers, waiting to be sent to the
/// client. Resolved once, and each client's throttle holds a clone.
pub(crate) static OUTGOING_THROTTLE_PERMITS_DAEMON: LazyLock<IntGauge> =
    LazyLock::new(|| OUTGOING_THROTTLE_PERMITS.with_label_values(&["daemon"]));

/// [`OUTGOING_THROTTLE_PERMITS`] child for data received from the client, waiting to be written to
/// remote peers. Resolved once, and each client's throttle holds a clone.
pub(crate) static OUTGOING_THROTTLE_PERMITS_CLIENT: LazyLock<IntGauge> =
    LazyLock::new(|| OUTGOING_THROTTLE_PERMITS.with_label_values(&["client"]));

/// Convenience trait for static metrics variables.
///
/// We store them as [`AtomicUsize`], which is the correct type (they're all counters).
/// However, [`IntGauge`] operates on i64.
trait AtomicUsizeExt {
    fn load_as_i64(&self) -> i64;
}

impl AtomicUsizeExt for AtomicUsize {
    fn load_as_i64(&self) -> i64 {
        self.load(Ordering::Relaxed).try_into().unwrap_or(i64::MAX)
    }
}

/// The state with all the metrics [`IntGauge`]s and the prometheus [`Registry`] where we keep them.
///
/// **Do not** modify the gauges directly!
///
/// Instead rely on [`Metrics::gather_metrics`], as we actually use a bunch of [`AtomicUsize`]s to
/// keep track of the values, they are the ones being (de|in)cremented. These gauges are just set
/// when it's time to send them via [`get_metrics`].
#[derive(Debug)]
struct Metrics {
    registry: &'static Registry,
    client_count: IntGauge,
    dns_request_count: IntGauge,
    open_fd_count: IntGauge,
    mirror_port_subscription: IntGauge,
    mirror_connection_subscription: IntGauge,
    steal_filtered_port_subscription: IntGauge,
    steal_unfiltered_port_subscription: IntGauge,
    redirected_connections: IntGauge,
    redirected_requests: IntGauge,
    tcp_outgoing_connection: IntGauge,
    udp_outgoing_connection: IntGauge,
}

impl Metrics {
    /// Creates a [`Registry`] to ... register our [`IntGauge`]s.
    fn new() -> Self {
        use prometheus::Opts;

        let registry = prometheus::default_registry();

        // Creates every `BUFFERED_MESSAGES_BYTES` and `OUTGOING_THROTTLE_PERMITS` series, so that
        // each reads 0 from the first scrape instead of appearing only after its first message.
        LazyLock::force(&BUFFERED_MESSAGE_GAUGES);
        LazyLock::force(&OUTGOING_THROTTLE_PERMITS_DAEMON);
        LazyLock::force(&OUTGOING_THROTTLE_PERMITS_CLIENT);

        let client_count = {
            let opts = Opts::new(
                "mirrord_agent_client_count",
                "amount of connected clients to this mirrord-agent",
            );
            IntGauge::with_opts(opts).expect("Valid at initialization!")
        };

        let dns_request_count = {
            let opts = Opts::new(
                "mirrord_agent_dns_request_count",
                "amount of in-progress dns requests in the mirrord-agent",
            );
            IntGauge::with_opts(opts).expect("Valid at initialization!")
        };

        let open_fd_count = {
            let opts = Opts::new(
                "mirrord_agent_open_fd_count",
                "amount of open file descriptors in mirrord-agent file manager",
            );
            IntGauge::with_opts(opts).expect("Valid at initialization!")
        };

        let mirror_port_subscription = {
            let opts = Opts::new(
                "mirrord_agent_mirror_port_subscription_count",
                "amount of mirror port subscriptions in mirror-agent",
            );
            IntGauge::with_opts(opts).expect("Valid at initialization!")
        };

        let mirror_connection_subscription = {
            let opts = Opts::new(
                "mirrord_agent_mirror_connection_subscription_count",
                "amount of connections in mirror mode in mirrord-agent",
            );
            IntGauge::with_opts(opts).expect("Valid at initialization!")
        };

        let steal_filtered_port_subscription = {
            let opts = Opts::new(
                "mirrord_agent_steal_filtered_port_subscription_count",
                "amount of filtered steal port subscriptions in mirrord-agent",
            );
            IntGauge::with_opts(opts).expect("Valid at initialization!")
        };

        let steal_unfiltered_port_subscription = {
            let opts = Opts::new(
                "mirrord_agent_steal_unfiltered_port_subscription_count",
                "amount of unfiltered steal port subscriptions in mirrord-agent",
            );
            IntGauge::with_opts(opts).expect("Valid at initialization!")
        };

        let redirected_connections = {
            let opts = Opts::new(
                "mirrord_agent_redirected_connections_count",
                "amount of incoming TCP connections currently redirected by mirrord-agent",
            );
            IntGauge::with_opts(opts).expect("Valid at initialization!")
        };

        let redirected_requests = {
            let opts = Opts::new(
                "mirrord_agent_redirected_requests_count",
                "amount of incoming HTTP requests currently redirected by mirrord-agent",
            );
            IntGauge::with_opts(opts).expect("Valid at initialization!")
        };

        let tcp_outgoing_connection = {
            let opts = Opts::new(
                "mirrord_agent_tcp_outgoing_connection_count",
                "amount of tcp outgoing connections in mirrord-agent",
            );
            IntGauge::with_opts(opts).expect("Valid at initialization!")
        };

        let udp_outgoing_connection = {
            let opts = Opts::new(
                "mirrord_agent_udp_outgoing_connection_count",
                "amount of udp outgoing connections in mirrord-agent",
            );
            IntGauge::with_opts(opts).expect("Valid at initialization!")
        };

        registry
            .register(Box::new(client_count.clone()))
            .expect("Register must be valid at initialization!");
        registry
            .register(Box::new(dns_request_count.clone()))
            .expect("Register must be valid at initialization!");
        registry
            .register(Box::new(open_fd_count.clone()))
            .expect("Register must be valid at initialization!");
        registry
            .register(Box::new(mirror_port_subscription.clone()))
            .expect("Register must be valid at initialization!");
        registry
            .register(Box::new(mirror_connection_subscription.clone()))
            .expect("Register must be valid at initialization!");
        registry
            .register(Box::new(steal_filtered_port_subscription.clone()))
            .expect("Register must be valid at initialization!");
        registry
            .register(Box::new(steal_unfiltered_port_subscription.clone()))
            .expect("Register must be valid at initialization!");
        registry
            .register(Box::new(redirected_connections.clone()))
            .expect("Register must be valid at initialization!");
        registry
            .register(Box::new(redirected_requests.clone()))
            .expect("Register must be valid at initialization!");
        registry
            .register(Box::new(tcp_outgoing_connection.clone()))
            .expect("Register must be valid at initialization!");
        registry
            .register(Box::new(udp_outgoing_connection.clone()))
            .expect("Register must be valid at initialization!");

        Self {
            registry,
            client_count,
            dns_request_count,
            open_fd_count,
            mirror_port_subscription,
            mirror_connection_subscription,
            steal_filtered_port_subscription,
            steal_unfiltered_port_subscription,
            redirected_connections,
            redirected_requests,
            tcp_outgoing_connection,
            udp_outgoing_connection,
        }
    }

    /// Calls [`IntGauge::set`] on every [`IntGauge`] of `Self`, setting it to the value of
    /// the corresponding [`AtomicUsize`] global (the uppercase named version of the gauge).
    ///
    /// Returns the list of [`MetricFamily`] registered in our [`Metrics::registry`], ready to be
    /// encoded and sent to prometheus.
    fn gather_metrics(&self) -> Vec<MetricFamily> {
        let Self {
            registry,
            client_count,
            dns_request_count,
            open_fd_count,
            mirror_port_subscription,
            mirror_connection_subscription,
            steal_filtered_port_subscription,
            steal_unfiltered_port_subscription,
            redirected_connections,
            redirected_requests,
            tcp_outgoing_connection,
            udp_outgoing_connection,
        } = self;

        client_count.set(CLIENT_COUNT.load_as_i64());
        dns_request_count.set(DNS_REQUEST_COUNT.load_as_i64());
        open_fd_count.set(OPEN_FD_COUNT.load_as_i64());
        mirror_port_subscription.set(MIRROR_PORT_SUBSCRIPTION.load_as_i64());
        mirror_connection_subscription.set(MIRROR_CONNECTION_SUBSCRIPTION.load_as_i64());
        steal_filtered_port_subscription.set(STEAL_FILTERED_PORT_SUBSCRIPTION.load_as_i64());
        steal_unfiltered_port_subscription.set(STEAL_UNFILTERED_PORT_SUBSCRIPTION.load_as_i64());
        redirected_connections.set(REDIRECTED_CONNECTIONS.load_as_i64());
        redirected_requests.set(REDIRECTED_REQUESTS.load_as_i64());
        tcp_outgoing_connection.set(TCP_OUTGOING_CONNECTION.load_as_i64());
        udp_outgoing_connection.set(UDP_OUTGOING_CONNECTION.load_as_i64());

        registry.gather()
    }
}

/// `GET /metrics`
///
/// Prepares all the metrics with [`Metrics::gather_metrics`], and responds to the prometheus
/// request.
#[tracing::instrument(level = Level::TRACE, ret)]
async fn get_metrics(State(state): State<Arc<Metrics>>) -> (StatusCode, String) {
    use prometheus::TextEncoder;

    let metric_families = state.gather_metrics();
    match TextEncoder.encode_to_string(&metric_families) {
        Ok(response) => (StatusCode::OK, response),
        Err(fail) => {
            tracing::error!(?fail, "Failed GET /metrics");
            (StatusCode::INTERNAL_SERVER_ERROR, fail.to_string())
        }
    }
}

/// Starts the mirrord-agent prometheus metrics service.
///
/// You can get the metrics from `GET address/metrics`.
///
/// - `address`: comes from a mirrord-agent config.
#[tracing::instrument(level = Level::TRACE, skip_all, ret ,err)]
pub(crate) async fn start_metrics(
    address: SocketAddr,
    cancellation_token: CancellationToken,
) -> Result<(), axum::BoxError> {
    let metrics_state = Arc::new(Metrics::new());

    let app = Router::new()
        .route("/metrics", get(get_metrics))
        .with_state(metrics_state);

    let listener = TcpListener::bind(address)
        .await
        .map_err(AgentError::from)
        .inspect_err(|fail| {
            tracing::error!(?fail, "Failed to bind TCP socket for metrics server")
        })?;

    let cancel_on_error = cancellation_token.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move { cancellation_token.cancelled().await })
        .await
        .inspect_err(|fail| {
            tracing::error!(%fail, "Could not start agent metrics server!");
            cancel_on_error.cancel();
        })?;

    Ok(())
}

/// A guard for a [`GaugeVec`] that decrements the metric value on `Drop`.
pub(crate) struct GaugeVecMetricGuard {
    /// The metric itself.
    metric: &'static GaugeVec,

    /// The **values** for the `labels` of this metric, i.e. `{port=80,id=1234}`.
    ///
    /// See the metric you're changing for the list of labels it expects, and pass them in the
    /// correct **order**.
    labels: Vec<String>,
}

impl GaugeVecMetricGuard {
    /// Increments the [`GaugeVec`] while setting its labels.
    ///
    /// ## Returns
    ///
    /// The `Self` guard that you should keep alive until you want this metric to be decremented.
    pub(crate) fn new(metric: &'static GaugeVec, labels: Vec<String>) -> Self {
        metric.with_label_values(labels.as_slice()).inc();

        Self { metric, labels }
    }
}

impl Drop for GaugeVecMetricGuard {
    fn drop(&mut self) {
        self.metric.with_label_values(self.labels.as_slice()).dec();
    }
}

/// Allows for managing metrics in a RAII fashion.
pub struct MetricGuard {
    metric: &'static AtomicUsize,
}

impl MetricGuard {
    pub fn new(metric: &'static AtomicUsize) -> Self {
        metric.fetch_add(1, Ordering::Relaxed);

        Self { metric }
    }
}

impl Drop for MetricGuard {
    fn drop(&mut self) {
        self.metric.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::atomic::Ordering, time::Duration};

    use tokio_util::sync::CancellationToken;

    use super::OPEN_FD_COUNT;
    use crate::metrics::start_metrics;

    #[tokio::test]
    async fn test_metrics() {
        let metrics_address = "127.0.0.1:9000".parse().unwrap();
        let cancellation_token = CancellationToken::new();

        let metrics_cancellation = cancellation_token.child_token();
        tokio::spawn(async move {
            start_metrics(metrics_address, metrics_cancellation)
                .await
                .unwrap()
        });

        OPEN_FD_COUNT.fetch_add(1, Ordering::Relaxed);

        // Give the server some time to start.
        tokio::time::sleep(Duration::from_secs(1)).await;

        let get_all_metrics = reqwest::get("http://127.0.0.1:9000/metrics")
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .text()
            .await
            .unwrap();

        assert!(get_all_metrics.contains("mirrord_agent_open_fd_count 1"));

        cancellation_token.drop_guard();
    }
}
