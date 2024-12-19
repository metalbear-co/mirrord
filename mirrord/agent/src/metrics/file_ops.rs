use kameo::message::{Context, Message};
use tracing::Level;

use crate::metrics::MetricsActor;

pub(crate) struct MetricsIncFd;
pub(crate) struct MetricsDecFd;

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
