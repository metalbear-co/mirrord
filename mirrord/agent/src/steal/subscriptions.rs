use std::{
    collections::{hash_map::Entry, HashMap},
    fmt,
    ops::Not,
    sync::atomic::Ordering,
};

use crate::{
    http::filter::HttpFilter,
    incoming::{RedirectorTaskError, StealHandle, StolenTraffic},
    metrics::{STEAL_FILTERED_PORT_SUBSCRIPTION, STEAL_UNFILTERED_PORT_SUBSCRIPTION},
    util::ClientId,
};

/// Set of active port subscriptions.
///
/// Responsible for managing port redirections and steal port subscription metrics.
pub struct PortSubscriptions {
    /// Used to request port redirections and fetch redirected connections.
    handle: StealHandle,
    /// Maps ports to active subscriptions.
    subscriptions: HashMap<u16, PortSubscription>,
}

impl Drop for PortSubscriptions {
    fn drop(&mut self) {
        let (filtered, unfiltered) =
            self.subscriptions
                .values()
                .fold(
                    (0, 0),
                    |(unfiltered, filtered), subscription| match subscription {
                        PortSubscription::Filtered(..) => (unfiltered, filtered + 1),
                        PortSubscription::Unfiltered(..) => (unfiltered + 1, filtered),
                    },
                );

        STEAL_UNFILTERED_PORT_SUBSCRIPTION.fetch_sub(unfiltered, Ordering::Relaxed);
        STEAL_FILTERED_PORT_SUBSCRIPTION.fetch_sub(filtered, Ordering::Relaxed);
    }
}

impl fmt::Debug for PortSubscriptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PortSubscriptions")
            .field("subscriptions", &self.subscriptions)
            .finish()
    }
}

impl PortSubscriptions {
    /// Create an empty instance of this struct.
    ///
    /// The given [`StealHandle`] will be used to enforce connection stealing according to the state
    /// of this set.
    pub fn new(handle: StealHandle) -> Self {
        Self {
            handle,
            subscriptions: Default::default(),
        }
    }

    /// Try adding a new subscription to this set.
    ///
    /// # Subscription clash rules
    ///
    /// * A single client may have only one subscription for the given port
    /// * A single port may have only one unfiltered subscription
    ///
    /// When a new subscription clashes with an existing one, the old one is replaced.
    ///
    /// # Params
    ///
    /// * `client_id` - identifier of the client that issued the subscription
    /// * `port` - number of the port to steal from
    /// * `filter` - optional [`HttpFilter`]
    pub async fn add(
        &mut self,
        client_id: ClientId,
        port: u16,
        filter: Option<HttpFilter>,
    ) -> Result<(), RedirectorTaskError> {
        let metric = if filter.is_some() {
            &STEAL_FILTERED_PORT_SUBSCRIPTION
        } else {
            &STEAL_UNFILTERED_PORT_SUBSCRIPTION
        };

        match self.subscriptions.entry(port) {
            Entry::Occupied(mut e) => {
                e.get_mut().merge(client_id, filter);
            }

            Entry::Vacant(e) => {
                self.handle.steal(port).await?;
                e.insert(PortSubscription::new(client_id, filter));
            }
        };

        metric.fetch_add(1, Ordering::Relaxed);

        Ok(())
    }

    /// Remove a subscription from this set, if it exists.
    ///
    /// # Params
    ///
    /// * `client_id` - identifier of the client that issued the subscription
    /// * `port` - number of the subscription port
    pub fn remove(&mut self, client_id: ClientId, port: u16) {
        let Entry::Occupied(mut e) = self.subscriptions.entry(port) else {
            return;
        };

        match e.get_mut() {
            PortSubscription::Unfiltered(subscribed_client) if *subscribed_client == client_id => {
                e.remove();
                STEAL_UNFILTERED_PORT_SUBSCRIPTION
                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

                self.handle.stop_steal(port);
            }
            PortSubscription::Unfiltered(..) => {}
            PortSubscription::Filtered(filters) => {
                if filters.remove(&client_id).is_some() {
                    STEAL_FILTERED_PORT_SUBSCRIPTION
                        .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                }

                if filters.is_empty() {
                    e.remove();
                    self.handle.stop_steal(port);
                }
            }
        }
    }

    /// Remove all client subscriptions from this set.
    ///
    /// # Params
    ///
    /// * `client_id` - identifier of the client that issued the subscriptions
    pub fn remove_all(&mut self, client_id: ClientId) {
        self.subscriptions
            .retain(|port, subscription| match subscription {
                PortSubscription::Unfiltered(subscribed_client)
                    if *subscribed_client == client_id =>
                {
                    STEAL_UNFILTERED_PORT_SUBSCRIPTION
                        .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

                    self.handle.stop_steal(*port);

                    false
                }
                PortSubscription::Unfiltered(..) => true,
                PortSubscription::Filtered(filters) => {
                    if filters.remove(&client_id).is_some() {
                        STEAL_FILTERED_PORT_SUBSCRIPTION
                            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                    }

                    if filters.is_empty() {
                        self.handle.stop_steal(*port);
                    }

                    filters.is_empty().not()
                }
            });
    }

    /// Wait until there's new stolen traffic available.
    pub async fn next(
        &mut self,
    ) -> Option<Result<(StolenTraffic, &PortSubscription), RedirectorTaskError>> {
        loop {
            let traffic = match self.handle.next().await? {
                Ok(traffic) => traffic,
                Err(err) => return Some(Err(err)),
            };

            let port = traffic.info().original_destination.port();
            if let Some(subscription) = self.subscriptions.get(&port) {
                break Some(Ok((traffic, subscription)));
            }

            tracing::warn!(
                ?traffic,
                "Received stolen traffic for a port that is no longer stolen, dropping",
            );
        }
    }
}

/// Steal subscription for a port.
#[derive(Debug)]
pub enum PortSubscription {
    /// No filter, incoming connections are stolen whole on behalf of the client.
    ///
    /// Belongs to a single client.
    Unfiltered(ClientId),
    /// Only HTTP requests matching one of the [`HttpFilter`]s should be stolen (on behalf of the
    /// filter owner).
    ///
    /// Can be shared by multiple clients.
    Filtered(HashMap<ClientId, HttpFilter>),
}

impl PortSubscription {
    /// Create a new instance. Variant is picked based on the optional `filter`.
    fn new(client_id: ClientId, filter: Option<HttpFilter>) -> Self {
        match filter {
            Some(filter) => Self::Filtered(HashMap::from_iter([(client_id, filter)])),
            None => Self::Unfiltered(client_id),
        }
    }

    /// Merge this subscription with a new one.
    fn merge(&mut self, client_id: ClientId, mut filter: Option<HttpFilter>) {
        if let Self::Filtered(filters) = self {
            if let Some(filter) = filter.take() {
                filters.insert(client_id, filter);
                return;
            }
        }

        *self = Self::new(client_id, filter);
    }
}

#[cfg(test)]
mod test {
    use crate::{
        http::filter::HttpFilter,
        incoming::{test::DummyRedirector, RedirectorTask},
        steal::subscriptions::{PortSubscription, PortSubscriptions},
        util::ClientId,
    };

    impl PortSubscription {
        /// Return whether this subscription belongs (possibly partially) to the given client.
        fn has_client(&self, client_id: ClientId) -> bool {
            match self {
                Self::Filtered(filters) => filters.contains_key(&client_id),
                Self::Unfiltered(subscribed_client) => *subscribed_client == client_id,
            }
        }
    }

    fn dummy_filter() -> HttpFilter {
        HttpFilter::Header(".*".parse().unwrap())
    }

    #[tokio::test]
    async fn multiple_subscriptions_one_port() {
        let (redirector, mut state, _tx) = DummyRedirector::new();
        let (redirector_task, steal_handle) = RedirectorTask::new(redirector, Default::default());
        tokio::spawn(redirector_task.run());
        let mut subscriptions = PortSubscriptions::new(steal_handle);

        // Adding unfiltered subscription.
        subscriptions.add(0, 80, None).await.unwrap();
        assert!(state.borrow().has_redirections([80]));
        let sub = subscriptions.subscriptions.get(&80).unwrap();
        assert!(matches!(sub, PortSubscription::Unfiltered(0)), "{sub:?}");

        // Another client's subscription should overwrite.
        subscriptions.add(1, 80, None).await.unwrap();
        assert!(state.borrow().has_redirections([80]));
        let sub = subscriptions.subscriptions.get(&80).unwrap();
        assert!(matches!(sub, PortSubscription::Unfiltered(1)), "{sub:?}");

        // Same client's next subscription should overwrite.
        subscriptions
            .add(1, 80, Some(dummy_filter()))
            .await
            .unwrap();
        assert!(state.borrow().has_redirections([80]));
        let sub = subscriptions.subscriptions.get(&80).unwrap();
        assert!(
            matches!(sub, PortSubscription::Filtered(filters) if filters.len() == 1 && sub.has_client(1)),
            "{sub:?}"
        );

        // Removing the subscription.
        subscriptions.remove(1, 80);

        // Checking if all is cleaned up.
        state
            .wait_for(|state| state.has_redirections([]))
            .await
            .unwrap();
        let sub = subscriptions.subscriptions.get(&80);
        assert!(sub.is_none(), "{sub:?}");
    }

    #[tokio::test]
    async fn multiple_subscriptions_multiple_ports() {
        let (redirector, mut state, _tx) = DummyRedirector::new();
        let (redirector_task, steal_handle) = RedirectorTask::new(redirector, Default::default());
        tokio::spawn(redirector_task.run());
        let mut subscriptions = PortSubscriptions::new(steal_handle);

        // Adding unfiltered subscription for port 80.
        subscriptions.add(0, 80, None).await.unwrap();

        // Adding filtered subscription for port 81.
        subscriptions
            .add(1, 81, Some(dummy_filter()))
            .await
            .unwrap();

        // Checking state.
        assert!(state.borrow().has_redirections([80, 81]));
        let sub = subscriptions.subscriptions.get(&80).unwrap();
        assert!(sub.has_client(0));
        assert!(matches!(sub, PortSubscription::Unfiltered(0)), "{sub:?}");
        let sub = subscriptions.subscriptions.get(&81).unwrap();
        assert!(sub.has_client(1));
        assert!(
            matches!(sub, PortSubscription::Filtered(filters) if filters.len() == 1),
            "{sub:?}"
        );

        // Removing subscriptions.
        subscriptions.remove(0, 80);
        subscriptions.remove(1, 81);

        // Checking if all is cleaned up.
        state
            .wait_for(|state| state.has_redirections([]))
            .await
            .unwrap();
        let sub = subscriptions.subscriptions.get(&80);
        assert!(sub.is_none(), "{sub:?}");
        let sub = subscriptions.subscriptions.get(&81);
        assert!(sub.is_none(), "{sub:?}");
    }

    #[tokio::test]
    async fn remove_all_from_client() {
        let (redirector, mut state, _tx) = DummyRedirector::new();
        let (redirector_task, steal_handle) = RedirectorTask::new(redirector, Default::default());
        tokio::spawn(redirector_task.run());
        let mut subscriptions = PortSubscriptions::new(steal_handle);

        // Adding unfiltered subscription for port 80.
        subscriptions.add(0, 80, None).await.unwrap();

        // Adding filtered subscription for port 81.
        subscriptions
            .add(0, 81, Some(dummy_filter()))
            .await
            .unwrap();

        // Checking state.
        assert!(state.borrow().has_redirections([80, 81]));
        let sub = subscriptions.subscriptions.get(&80).unwrap();
        assert!(sub.has_client(0));
        assert!(matches!(sub, PortSubscription::Unfiltered(0)), "{sub:?}");
        let sub = subscriptions.subscriptions.get(&81).unwrap();
        assert!(sub.has_client(0));
        assert!(
            matches!(sub, PortSubscription::Filtered(filters) if filters.len() == 1),
            "{sub:?}"
        );

        // Removing all subscriptions of a client.
        subscriptions.remove_all(0);

        // Checking if all is cleaned up.
        state
            .wait_for(|state| state.has_redirections([]))
            .await
            .unwrap();
        let sub = subscriptions.subscriptions.get(&80);
        assert!(sub.is_none(), "{sub:?}");
        let sub = subscriptions.subscriptions.get(&81);
        assert!(sub.is_none(), "{sub:?}");
    }
}
