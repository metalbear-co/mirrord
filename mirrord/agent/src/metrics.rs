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
    open_fds_count: u64,
    connected_clients_count: u64,
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

pub(crate) struct MetricsIncrementFd;
pub(crate) struct MetricsDecrementFd;
pub(crate) struct MetricsClientConnected;
pub(crate) struct MetricsClientDisconnected;
pub(crate) struct MetricsGetAll;

#[derive(Reply, Serialize)]
pub(crate) struct MetricsGetAllReply {
    open_fds_count: u64,
    connected_clients_count: u64,
}

impl Message<MetricsIncrementFd> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::INFO, skip_all)]
    async fn handle(
        &mut self,
        _: MetricsIncrementFd,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.open_fds_count += 1;
    }
}

impl Message<MetricsDecrementFd> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::INFO, skip_all)]
    async fn handle(
        &mut self,
        _: MetricsDecrementFd,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.open_fds_count = self.open_fds_count.saturating_sub(1);
    }
}

impl Message<MetricsClientConnected> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::INFO, skip_all)]
    async fn handle(
        &mut self,
        _: MetricsClientConnected,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.connected_clients_count += 1;
    }
}

impl Message<MetricsClientDisconnected> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::INFO, skip_all)]
    async fn handle(
        &mut self,
        _: MetricsClientDisconnected,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.connected_clients_count = self.connected_clients_count.saturating_sub(1);
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
            open_fds_count: self.open_fds_count,
            connected_clients_count: self.connected_clients_count,
        }
    }
}
