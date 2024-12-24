use std::{net::SocketAddr, sync::LazyLock};

use axum::{response::IntoResponse, routing::get, Router};
use prometheus::{register_int_gauge, IntGauge};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::Level;

use crate::error::AgentError;

pub(crate) static OPEN_FD_COUNT: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!(
        "mirrord_agent_open_fd_count",
        "amount of open file descriptors in mirrord-agent file manager"
    )
    .expect("Valid at initialization!")
});

pub(crate) static MIRROR_PORT_SUBSCRIPTION: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!(
        "mirrord_agent_mirror_port_subscription_count",
        "amount of mirror port subscriptions in mirror-agent"
    )
    .expect("Valid at initialization")
});

pub(crate) static MIRROR_CONNECTION_SUBSCRIPTION: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!(
        "mirrord_agent_mirror_connection_subscription_count",
        "amount of connections in mirror mode in mirrord-agent"
    )
    .expect("Valid at initialization!")
});

pub(crate) static STEAL_FILTERED_PORT_SUBSCRIPTION: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!(
        "mirrord_agent_steal_filtered_port_subscription_count",
        "amount of filtered steal port subscriptions in mirrord-agent"
    )
    .expect("Valid at initialization!")
});

pub(crate) static STEAL_UNFILTERED_PORT_SUBSCRIPTION: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!(
        "mirrord_agent_steal_unfiltered_port_subscription_count",
        "amount of unfiltered steal port subscriptions in mirrord-agent"
    )
    .expect("Valid at initialization!")
});

pub(crate) static STEAL_FILTERED_CONNECTION_SUBSCRIPTION: LazyLock<IntGauge> =
    LazyLock::new(|| {
        register_int_gauge!(
            "mirrord_agent_steal_connection_subscription_count",
            "amount of filtered connections in steal mode in mirrord-agent"
        )
        .expect("Valid at initialization!")
    });

pub(crate) static STEAL_UNFILTERED_CONNECTION_SUBSCRIPTION: LazyLock<IntGauge> =
    LazyLock::new(|| {
        register_int_gauge!(
            "mirrord_agent_steal_connection_subscription_count",
            "amount of unfiltered connections in steal mode in mirrord-agent"
        )
        .expect("Valid at initialization!")
    });

pub(crate) static TCP_OUTGOING_CONNECTION: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!(
        "mirrord_agent_tcp_outgoing_connection_count",
        "amount of tcp outgoing connections in mirrord-agent"
    )
    .expect("Valid at initialization!")
});

pub(crate) static UDP_OUTGOING_CONNECTION: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!(
        "mirrord_agent_udp_outgoing_connection_count",
        "amount of udp outgoing connections in mirrord-agent"
    )
    .expect("Valid at initialization!")
});

#[derive(Error, Debug)]
pub(crate) enum MetricsError {
    #[error(transparent)]
    FromUtf8(#[from] std::string::FromUtf8Error),

    #[error(transparent)]
    Prometheus(#[from] prometheus::Error),
}

impl IntoResponse for MetricsError {
    fn into_response(self) -> axum::response::Response {
        (http::StatusCode::INTERNAL_SERVER_ERROR, self.to_string()).into_response()
    }
}

#[tracing::instrument(level = Level::TRACE,  ret, err)]
async fn get_metrics() -> Result<String, MetricsError> {
    use prometheus::TextEncoder;

    let metric_families = prometheus::gather();

    Ok(TextEncoder.encode_to_string(&metric_families)?)
}

#[tracing::instrument(level = Level::TRACE, skip_all, ret ,err)]
pub(crate) async fn start_metrics(
    address: SocketAddr,
    cancellation_token: CancellationToken,
) -> Result<(), axum::BoxError> {
    let app = Router::new().route("/metrics", get(get_metrics));

    let listener = TcpListener::bind(address)
        .await
        .map_err(AgentError::from)
        .inspect_err(|fail| tracing::error!(?fail, "Failed to bind TCP socket for metrics server"))?;

    let cancel_on_error = cancellation_token.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move { cancellation_token.cancelled().await })
        .await
        .inspect_err(|fail| {
            tracing::error!(%fail, "Could not start agent metrics
        server!");
            cancel_on_error.cancel();
        })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

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

        OPEN_FD_COUNT.inc();

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
