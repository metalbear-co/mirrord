use kameo::message::{Context, Message};
use tracing::Level;

use crate::metrics::MetricsActor;

pub(crate) struct MetricsIncTcpOutgoingConnection;
pub(crate) struct MetricsDecTcpOutgoingConnection;

pub(crate) struct MetricsIncUdpOutgoingConnection;
pub(crate) struct MetricsDecUdpOutgoingConnection;

impl Message<MetricsIncTcpOutgoingConnection> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::TRACE, skip_all)]
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

    #[tracing::instrument(level = Level::TRACE, skip_all)]
    async fn handle(
        &mut self,
        _: MetricsDecTcpOutgoingConnection,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.tcp_outgoing_connection_count = self.tcp_outgoing_connection_count.saturating_sub(1);
    }
}

impl Message<MetricsIncUdpOutgoingConnection> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::TRACE, skip_all)]
    async fn handle(
        &mut self,
        _: MetricsIncUdpOutgoingConnection,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.udp_outgoing_connection_count += 1;
    }
}

impl Message<MetricsDecUdpOutgoingConnection> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::TRACE, skip_all)]
    async fn handle(
        &mut self,
        _: MetricsDecUdpOutgoingConnection,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.udp_outgoing_connection_count = self.udp_outgoing_connection_count.saturating_sub(1);
    }
}
