use axum::{response::IntoResponse, routing::get, Extension, Router};
use kameo::{
    actor::ActorRef,
    error::BoxError,
    mailbox::unbounded::UnboundedMailbox,
    message::{Context, Message},
    Actor, Reply,
};
use prometheus::core::{AtomicI64, GenericGauge};
use serde::Serialize;
use thiserror::Error;
use tokio::net::TcpListener;
use tracing::Level;

use crate::error::AgentError;

#[derive(Error, Debug)]
pub(crate) enum MetricsError {
    #[error(transparent)]
    GetAll(#[from] kameo::error::SendError<MetricsGetAll>),

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

#[tracing::instrument(level = Level::INFO, skip(prometheus_metrics), ret, err)]
#[axum::debug_handler]
async fn get_metrics(
    metrics: Extension<ActorRef<MetricsActor>>,
    prometheus_metrics: Extension<PrometheusMetrics>,
) -> Result<String, MetricsError> {
    use prometheus::{Encoder, TextEncoder};

    let MetricsGetAllReply {
        open_fd_count,
        mirror_port_subscription_count,
        mirror_connection_subscription_count,
        steal_filtered_port_subscription_count,
        steal_unfiltered_port_subscription_count,
        steal_connection_subscription_count,
        tcp_outgoing_connection_count,
    } = metrics.ask(MetricsGetAll).await?;

    prometheus_metrics.open_fd_count.set(open_fd_count as i64);

    prometheus_metrics
        .mirror_port_subscription_count
        .set(mirror_port_subscription_count as i64);

    prometheus_metrics
        .mirror_connection_subscription_count
        .set(mirror_connection_subscription_count as i64);

    prometheus_metrics
        .steal_filtered_port_subscription_count
        .set(steal_filtered_port_subscription_count as i64);

    prometheus_metrics
        .steal_unfiltered_port_subscription_count
        .set(steal_unfiltered_port_subscription_count as i64);

    prometheus_metrics
        .steal_connection_subscription_count
        .set(steal_connection_subscription_count as i64);

    prometheus_metrics
        .tcp_outgoing_connection_count
        .set(tcp_outgoing_connection_count as i64);

    let metric_families = prometheus::gather();

    let mut buffer = Vec::new();
    TextEncoder
        .encode(&metric_families, &mut buffer)
        .inspect_err(|error| tracing::error!(%error, "unable to encode prometheus metrics"))?;

    Ok(String::from_utf8(buffer)?)
}

#[derive(Clone)]
struct PrometheusMetrics {
    open_fd_count: GenericGauge<AtomicI64>,
    mirror_port_subscription_count: GenericGauge<AtomicI64>,
    mirror_connection_subscription_count: GenericGauge<AtomicI64>,
    steal_filtered_port_subscription_count: GenericGauge<AtomicI64>,
    steal_unfiltered_port_subscription_count: GenericGauge<AtomicI64>,
    steal_connection_subscription_count: GenericGauge<AtomicI64>,
    tcp_outgoing_connection_count: GenericGauge<AtomicI64>,
}

impl PrometheusMetrics {
    fn new() -> Result<Self, MetricsError> {
        use prometheus::register_int_gauge;

        Ok(Self {
            open_fd_count: register_int_gauge!(
                "mirrord_agent_open_fd_count",
                "amount of open fds in mirrord-agent"
            )?,
            mirror_port_subscription_count: register_int_gauge!(
                "mirrord_agent_mirror_port_subscription_count",
                "amount of mirror port subscriptions in mirror-agent"
            )?,
            mirror_connection_subscription_count: register_int_gauge!(
                "mirrord_agent_mirror_connection_subscription_count",
                "amount of connections in steal mode in mirrord-agent"
            )?,
            steal_filtered_port_subscription_count: register_int_gauge!(
                "mirrord_agent_steal_filtered_port_subscription_count",
                "amount of filtered steal port subscriptions in mirrord-agent"
            )?,
            steal_unfiltered_port_subscription_count: register_int_gauge!(
                "mirrord_agent_steal_unfiltered_port_subscription_count",
                "amount of unfiltered steal port subscriptions in mirrord-agent"
            )?,
            steal_connection_subscription_count: register_int_gauge!(
                "mirrord_agent_steal_connection_subscription_count",
                "amount of connections in steal mode in mirrord-agent"
            )?,
            tcp_outgoing_connection_count: register_int_gauge!(
                "mirrord_agent_tcp_outgoing_connection_count",
                "amount of tcp outgoing connections in mirrord-agent"
            )?,
        })
    }
}

#[derive(Default)]
pub(crate) struct MetricsActor {
    enabled: bool,
    open_fd_count: u64,
    mirror_port_subscription_count: u64,
    mirror_connection_subscription_count: u64,
    steal_filtered_port_subscription_count: u64,
    steal_unfiltered_port_subscription_count: u64,
    steal_connection_subscription_count: u64,
    tcp_outgoing_connection_count: u64,
}

impl MetricsActor {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            ..Default::default()
        }
    }
}

impl Actor for MetricsActor {
    type Mailbox = UnboundedMailbox<Self>;

    #[tracing::instrument(level = Level::INFO, skip_all, ret ,err)]
    async fn on_start(&mut self, metrics: ActorRef<Self>) -> Result<(), BoxError> {
        if self.enabled {
            let prometheus_metrics = PrometheusMetrics::new()?;

            let app = Router::new()
                .route("/metrics", get(get_metrics))
                .layer(Extension(metrics))
                .layer(Extension(prometheus_metrics));

            let listener = TcpListener::bind("0.0.0.0:9000")
                .await
                .map_err(AgentError::from)
                .inspect_err(|fail| tracing::error!(?fail, "Actor listener!"))?;

            tokio::spawn(async move {
                axum::serve(listener, app).await.inspect_err(|fail| {
                    tracing::error!(%fail, "Could not start agent metrics
        server!")
                })
            });
        }

        Ok(())
    }
}

pub(crate) struct MetricsIncFd;
pub(crate) struct MetricsDecFd;

pub(crate) struct MetricsIncMirrorPortSubscription;
pub(crate) struct MetricsDecMirrorPortSubscription;

pub(crate) struct MetricsIncMirrorConnectionSubscription;
pub(crate) struct MetricsDecMirrorConnectionSubscription;

pub(crate) struct MetricsIncStealPortSubscription {
    pub(crate) filtered: bool,
}
pub(crate) struct MetricsDecStealPortSubscription {
    pub(crate) filtered: bool,
}

pub(crate) struct MetricsDecStealPortSubscriptionMany {
    pub(crate) removed_subscriptions: Vec<bool>,
}

pub(crate) struct MetricsIncStealConnectionSubscription;
pub(crate) struct MetricsDecStealConnectionSubscription;

pub(crate) struct MetricsIncTcpOutgoingConnection;
pub(crate) struct MetricsDecTcpOutgoingConnection;

pub(crate) struct MetricsGetAll;

#[derive(Reply, Serialize)]
pub(crate) struct MetricsGetAllReply {
    open_fd_count: u64,
    mirror_port_subscription_count: u64,
    mirror_connection_subscription_count: u64,
    steal_filtered_port_subscription_count: u64,
    steal_unfiltered_port_subscription_count: u64,
    steal_connection_subscription_count: u64,
    tcp_outgoing_connection_count: u64,
}

impl Message<MetricsIncFd> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::INFO, skip_all)]
    async fn handle(
        &mut self,
        _: MetricsIncFd,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.open_fd_count += 1;
    }
}

impl Message<MetricsDecFd> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::INFO, skip_all)]
    async fn handle(
        &mut self,
        _: MetricsDecFd,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.open_fd_count = self.open_fd_count.saturating_sub(1);
    }
}

impl Message<MetricsIncMirrorPortSubscription> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::INFO, skip_all)]
    async fn handle(
        &mut self,
        _: MetricsIncMirrorPortSubscription,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.mirror_port_subscription_count += 1;
    }
}

impl Message<MetricsDecMirrorPortSubscription> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::INFO, skip_all)]
    async fn handle(
        &mut self,
        _: MetricsDecMirrorPortSubscription,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.mirror_port_subscription_count = self.mirror_port_subscription_count.saturating_sub(1);
    }
}

impl Message<MetricsIncStealPortSubscription> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::INFO, skip_all)]
    async fn handle(
        &mut self,
        MetricsIncStealPortSubscription { filtered }: MetricsIncStealPortSubscription,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        if filtered {
            self.steal_filtered_port_subscription_count += 1;
        } else {
            self.steal_unfiltered_port_subscription_count += 1;
        }
    }
}

impl Message<MetricsDecStealPortSubscription> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::INFO, skip_all)]
    async fn handle(
        &mut self,
        MetricsDecStealPortSubscription { filtered }: MetricsDecStealPortSubscription,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        if filtered {
            self.steal_filtered_port_subscription_count = self
                .steal_filtered_port_subscription_count
                .saturating_sub(1);
        } else {
            self.steal_unfiltered_port_subscription_count = self
                .steal_unfiltered_port_subscription_count
                .saturating_sub(1);
        }
    }
}

impl Message<MetricsDecStealPortSubscriptionMany> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::INFO, skip_all)]
    async fn handle(
        &mut self,
        MetricsDecStealPortSubscriptionMany {
            removed_subscriptions,
        }: MetricsDecStealPortSubscriptionMany,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        for filtered in removed_subscriptions {
            if filtered {
                self.steal_filtered_port_subscription_count = self
                    .steal_filtered_port_subscription_count
                    .saturating_sub(1);
            } else {
                self.steal_unfiltered_port_subscription_count = self
                    .steal_unfiltered_port_subscription_count
                    .saturating_sub(1);
            }
        }
    }
}

impl Message<MetricsIncStealConnectionSubscription> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::INFO, skip_all)]
    async fn handle(
        &mut self,
        _: MetricsIncStealConnectionSubscription,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.steal_connection_subscription_count += 1;
    }
}

impl Message<MetricsDecStealConnectionSubscription> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::INFO, skip_all)]
    async fn handle(
        &mut self,
        _: MetricsDecStealConnectionSubscription,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.steal_connection_subscription_count =
            self.steal_connection_subscription_count.saturating_sub(1);
    }
}

impl Message<MetricsIncMirrorConnectionSubscription> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::INFO, skip_all)]
    async fn handle(
        &mut self,
        _: MetricsIncMirrorConnectionSubscription,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.mirror_connection_subscription_count += 1;
    }
}

impl Message<MetricsDecMirrorConnectionSubscription> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::INFO, skip_all)]
    async fn handle(
        &mut self,
        _: MetricsDecMirrorConnectionSubscription,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.mirror_connection_subscription_count =
            self.mirror_connection_subscription_count.saturating_sub(1);
    }
}

impl Message<MetricsIncTcpOutgoingConnection> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::INFO, skip_all)]
    async fn handle(
        &mut self,
        _: MetricsIncTcpOutgoingConnection,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.tcp_outgoing_connection_count += 1;
    }
}

impl Message<MetricsDecTcpOutgoingConnection> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::INFO, skip_all)]
    async fn handle(
        &mut self,
        _: MetricsDecTcpOutgoingConnection,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.tcp_outgoing_connection_count = self.tcp_outgoing_connection_count.saturating_sub(1);
    }
}

impl Message<MetricsGetAll> for MetricsActor {
    type Reply = MetricsGetAllReply;

    #[tracing::instrument(level = Level::INFO, skip_all)]
    async fn handle(
        &mut self,
        _: MetricsGetAll,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        MetricsGetAllReply {
            open_fd_count: self.open_fd_count,
            mirror_port_subscription_count: self.mirror_port_subscription_count,
            mirror_connection_subscription_count: self.mirror_connection_subscription_count,
            steal_filtered_port_subscription_count: self.steal_filtered_port_subscription_count,
            steal_unfiltered_port_subscription_count: self.steal_unfiltered_port_subscription_count,
            steal_connection_subscription_count: self.steal_connection_subscription_count,
            tcp_outgoing_connection_count: self.tcp_outgoing_connection_count,
        }
    }
}
