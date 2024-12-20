use std::{net::SocketAddr, sync::LazyLock};

use axum::{response::IntoResponse, routing::get, Router};
use prometheus::{register_int_gauge, IntGauge};
use thiserror::Error;
use tokio::net::TcpListener;
use tracing::Level;

use crate::error::AgentError;

pub(crate) static OPEN_FD_COUNT: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!(
        "mirrord_agent_open_fd_count",
        "amount of open fds in mirrord-agent"
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
        "amount of connections in steal mode in mirrord-agent"
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

pub(crate) static STEAL_CONNECTION_SUBSCRIPTION: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!(
        "mirrord_agent_steal_connection_subscription_count",
        "amount of connections in steal mode in mirrord-agent"
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

unsafe impl Send for MetricsError {}

impl IntoResponse for MetricsError {
    fn into_response(self) -> axum::response::Response {
        (http::StatusCode::INTERNAL_SERVER_ERROR, self.to_string()).into_response()
    }
}

#[tracing::instrument(level = Level::TRACE,  ret, err)]
async fn get_metrics() -> Result<String, MetricsError> {
    use prometheus::{Encoder, TextEncoder};

    let metric_families = prometheus::gather();

    let mut buffer = Vec::new();
    TextEncoder
        .encode(&metric_families, &mut buffer)
        .inspect_err(|error| tracing::error!(%error, "unable to encode prometheus metrics"))?;

    Ok(String::from_utf8(buffer)?)
}

#[tracing::instrument(level = Level::TRACE, skip_all, ret ,err)]
pub(crate) async fn start_metrics(address: SocketAddr) -> Result<(), axum::BoxError> {
    let app = Router::new().route("/metrics", get(get_metrics));

    let listener = TcpListener::bind(address)
        .await
        .map_err(AgentError::from)
        .inspect_err(|fail| tracing::error!(?fail, "Actor listener!"))?;

    tokio::spawn(async move {
        axum::serve(listener, app).await.inspect_err(|fail| {
            tracing::error!(%fail, "Could not start agent metrics
        server!")
        })
    });

    Ok(())
}
