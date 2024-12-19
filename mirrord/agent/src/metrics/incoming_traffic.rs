use kameo::message::{Context, Message};
use tracing::Level;

use crate::metrics::MetricsActor;

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

impl Message<MetricsIncMirrorPortSubscription> for MetricsActor {
    type Reply = ();

    #[tracing::instrument(level = Level::TRACE, skip_all)]
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

    #[tracing::instrument(level = Level::TRACE, skip_all)]
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

    #[tracing::instrument(level = Level::TRACE, skip_all)]
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

    #[tracing::instrument(level = Level::TRACE, skip_all)]
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

    #[tracing::instrument(level = Level::TRACE, skip_all)]
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

    #[tracing::instrument(level = Level::TRACE, skip_all)]
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

    #[tracing::instrument(level = Level::TRACE, skip_all)]
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

    #[tracing::instrument(level = Level::TRACE, skip_all)]
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

    #[tracing::instrument(level = Level::TRACE, skip_all)]
    async fn handle(
        &mut self,
        _: MetricsDecMirrorConnectionSubscription,
        _ctx: Context<'_, Self, Self::Reply>,
    ) -> Self::Reply {
        self.mirror_connection_subscription_count =
            self.mirror_connection_subscription_count.saturating_sub(1);
    }
}
