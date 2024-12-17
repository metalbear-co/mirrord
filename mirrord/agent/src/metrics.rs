use axum::{response::IntoResponse, routing::get, Extension, Router};
use kameo::{
    actor::ActorRef,
    error::BoxError,
    mailbox::unbounded::UnboundedMailbox,
    message::{Context, Message},
    Actor, Reply,
};
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

impl IntoResponse for MetricsError {
    fn into_response(self) -> axum::response::Response {
        (http::StatusCode::INTERNAL_SERVER_ERROR, self.to_string()).into_response()
    }
}

#[tracing::instrument(level = Level::INFO, ret, err)]
async fn get_metrics(metrics: Extension<ActorRef<MetricsActor>>) -> Result<String, MetricsError> {
    use prometheus::{register_int_gauge, Encoder, TextEncoder};

    let MetricsGetAllReply {
        open_fds_count,
        connected_clients_count,
    } = metrics.ask(MetricsGetAll).await?;

    register_int_gauge!(
        "mirrord_agent_open_fds_count",
        "amount of open fds in mirrord-agent"
    )?
    .set(open_fds_count as i64);

    register_int_gauge!(
        "mirrord_agent_connected_clients_count",
        "amount of connected clients in mirrord-agent"
    )?
    .set(connected_clients_count as i64);

    let metric_families = prometheus::gather();

    let mut buffer = Vec::new();
    TextEncoder
        .encode(&metric_families, &mut buffer)
        .inspect_err(|error| tracing::error!(%error, "unable to encode prometheus metrics"))?;

    Ok(String::from_utf8(buffer)?)
}

#[derive(Default)]
pub(crate) struct MetricsActor {
    enabled: bool,
    open_fd_count: u64,
    connected_client_count: u64,
    port_subscription_count: u64,
    connection_subscription_count: u64,
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
            let app = Router::new()
                .route("/metrics", get(get_metrics))
                .layer(Extension(metrics));

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

pub(crate) struct MetricsIncClient;
pub(crate) struct MetricsDecClient;

pub(crate) struct MetricsIncPortSubscription;
pub(crate) struct MetricsDecPortSubscription;

pub(crate) struct MetricsIncConnectionSubscription;
pub(crate) struct MetricsDecConnectionSubscription;

pub(crate) struct MetricsGetAll;

#[derive(Reply, Serialize)]
pub(crate) struct MetricsGetAllReply {
    open_fds_count: u64,
    connected_clients_count: u64,
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

impl Message<MetricsIncClient> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::INFO, skip_all)]
    async fn handle(
        &mut self,
        _: MetricsIncClient,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.connected_client_count += 1;
    }
}

impl Message<MetricsDecClient> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::INFO, skip_all)]
    async fn handle(
        &mut self,
        _: MetricsDecClient,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.connected_client_count = self.connected_client_count.saturating_sub(1);
    }
}

impl Message<MetricsIncPortSubscription> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::INFO, skip_all)]
    async fn handle(
        &mut self,
        _: MetricsIncPortSubscription,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.port_subscription_count += 1;
    }
}

impl Message<MetricsDecPortSubscription> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::INFO, skip_all)]
    async fn handle(
        &mut self,
        _: MetricsDecPortSubscription,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.port_subscription_count = self.port_subscription_count.saturating_sub(1);
    }
}

impl Message<MetricsIncConnectionSubscription> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::INFO, skip_all)]
    async fn handle(
        &mut self,
        _: MetricsIncConnectionSubscription,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.connection_subscription_count += 1;
    }
}

impl Message<MetricsDecConnectionSubscription> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::INFO, skip_all)]
    async fn handle(
        &mut self,
        _: MetricsDecConnectionSubscription,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.connection_subscription_count = self.connection_subscription_count.saturating_sub(1);
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
            open_fds_count: self.open_fd_count,
            connected_clients_count: self.connected_client_count,
        }
    }
}
