use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicI64, AtomicUsize},
        Arc,
    },
};

use axum::{extract::State, routing::get, Router};
use http::StatusCode;
use prometheus::{proto::MetricFamily, IntGauge, Registry};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::Level;

use crate::error::AgentError;

/// Incremented whenever we get a new client in `ClientConnectionHandler`, and decremented
/// when this client is dropped.
pub(crate) static CLIENT_COUNT: AtomicI64 = AtomicI64::new(0);

/// How many DNS resolution client requests the agent is currently handling.
pub(crate) static DNS_REQUEST_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Incremented and decremented in _open-ish_/_close-ish_ file operations in `FileManager`,
/// Also gets decremented when `FileManager` is dropped.
pub(crate) static OPEN_FD_COUNT: AtomicI64 = AtomicI64::new(0);

/// Follows the amount of subscribed ports in `update_packet_filter`. We don't really
/// increment/decrement this one, and mostly `set` it to the latest amount of ports, zeroing it when
/// the `TcpConnectionSniffer` gets dropped.
pub(crate) static MIRROR_PORT_SUBSCRIPTION: AtomicI64 = AtomicI64::new(0);

pub(crate) static MIRROR_CONNECTION_SUBSCRIPTION: AtomicI64 = AtomicI64::new(0);

pub(crate) static STEAL_FILTERED_PORT_SUBSCRIPTION: AtomicI64 = AtomicI64::new(0);

pub(crate) static STEAL_UNFILTERED_PORT_SUBSCRIPTION: AtomicI64 = AtomicI64::new(0);

pub(crate) static STEAL_FILTERED_CONNECTION_SUBSCRIPTION: AtomicI64 = AtomicI64::new(0);

pub(crate) static STEAL_UNFILTERED_CONNECTION_SUBSCRIPTION: AtomicI64 = AtomicI64::new(0);

pub(crate) static HTTP_REQUEST_IN_PROGRESS_COUNT: AtomicI64 = AtomicI64::new(0);

pub(crate) static TCP_OUTGOING_CONNECTION: AtomicI64 = AtomicI64::new(0);

pub(crate) static UDP_OUTGOING_CONNECTION: AtomicI64 = AtomicI64::new(0);

/// The state with all the metrics [`IntGauge`]s and the prometheus [`Registry`] where we keep them.
///
/// **Do not** modify the gauges directly!
///
/// Instead rely on [`Metrics::gather_metrics`], as we actually use a bunch of [`AtomicI64`]s to
/// keep track of the values, they are the ones being (de|in)cremented. These gauges are just set
/// when it's time to send them via [`get_metrics`].
#[derive(Debug)]
struct Metrics {
    registry: Registry,
    client_count: IntGauge,
    dns_request_count: IntGauge,
    open_fd_count: IntGauge,
    mirror_port_subscription: IntGauge,
    mirror_connection_subscription: IntGauge,
    steal_filtered_port_subscription: IntGauge,
    steal_unfiltered_port_subscription: IntGauge,
    steal_filtered_connection_subscription: IntGauge,
    steal_unfiltered_connection_subscription: IntGauge,
    http_request_in_progress_count: IntGauge,
    tcp_outgoing_connection: IntGauge,
    udp_outgoing_connection: IntGauge,
}

impl Metrics {
    /// Creates a [`Registry`] to ... register our [`IntGauge`]s.
    fn new() -> Self {
        use prometheus::Opts;

        let registry = Registry::new();

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

        let steal_filtered_connection_subscription = {
            let opts = Opts::new(
                "mirrord_agent_steal_connection_subscription_count",
                "amount of filtered connections in steal mode in mirrord-agent",
            );
            IntGauge::with_opts(opts).expect("Valid at initialization!")
        };

        let steal_unfiltered_connection_subscription = {
            let opts = Opts::new(
                "mirrord_agent_steal_unfiltered_connection_subscription_count",
                "amount of unfiltered connections in steal mode in mirrord-agent",
            );
            IntGauge::with_opts(opts).expect("Valid at initialization!")
        };

        let http_request_in_progress_count = {
            let opts = Opts::new(
                "mirrord_agent_http_request_in_progress_count",
                "amount of in-progress http requests in the mirrord-agent",
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
            .register(Box::new(steal_filtered_connection_subscription.clone()))
            .expect("Register must be valid at initialization!");
        registry
            .register(Box::new(steal_unfiltered_connection_subscription.clone()))
            .expect("Register must be valid at initialization!");
        registry
            .register(Box::new(http_request_in_progress_count.clone()))
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
            steal_filtered_connection_subscription,
            steal_unfiltered_connection_subscription,
            http_request_in_progress_count,
            tcp_outgoing_connection,
            udp_outgoing_connection,
        }
    }

    /// Calls [`IntGauge::set`] on every [`IntGauge`] of `Self`, setting it to the value of
    /// the corresponding [`AtomicI64`] global (the uppercase named version of the gauge).
    ///
    /// Returns the list of [`MetricFamily`] registered in our [`Metrics::registry`], ready to be
    /// encoded and sent to prometheus.
    fn gather_metrics(&self) -> Vec<MetricFamily> {
        use std::sync::atomic::Ordering;

        let Self {
            registry,
            client_count,
            dns_request_count,
            open_fd_count,
            mirror_port_subscription,
            mirror_connection_subscription,
            steal_filtered_port_subscription,
            steal_unfiltered_port_subscription,
            steal_filtered_connection_subscription,
            steal_unfiltered_connection_subscription,
            http_request_in_progress_count,
            tcp_outgoing_connection,
            udp_outgoing_connection,
        } = self;

        client_count.set(CLIENT_COUNT.load(Ordering::Relaxed));
        dns_request_count.set(
            DNS_REQUEST_COUNT
                .load(Ordering::Relaxed)
                .try_into()
                .unwrap_or(i64::MAX),
        );
        open_fd_count.set(OPEN_FD_COUNT.load(Ordering::Relaxed));
        mirror_port_subscription.set(MIRROR_PORT_SUBSCRIPTION.load(Ordering::Relaxed));
        mirror_connection_subscription.set(MIRROR_CONNECTION_SUBSCRIPTION.load(Ordering::Relaxed));
        steal_filtered_port_subscription
            .set(STEAL_FILTERED_PORT_SUBSCRIPTION.load(Ordering::Relaxed));
        steal_unfiltered_port_subscription
            .set(STEAL_UNFILTERED_PORT_SUBSCRIPTION.load(Ordering::Relaxed));
        steal_filtered_connection_subscription
            .set(STEAL_FILTERED_CONNECTION_SUBSCRIPTION.load(Ordering::Relaxed));
        steal_unfiltered_connection_subscription
            .set(STEAL_UNFILTERED_CONNECTION_SUBSCRIPTION.load(Ordering::Relaxed));
        http_request_in_progress_count.set(HTTP_REQUEST_IN_PROGRESS_COUNT.load(Ordering::Relaxed));
        tcp_outgoing_connection.set(TCP_OUTGOING_CONNECTION.load(Ordering::Relaxed));
        udp_outgoing_connection.set(UDP_OUTGOING_CONNECTION.load(Ordering::Relaxed));

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
