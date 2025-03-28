use std::{
    collections::{hash_map::Entry, HashMap},
    ops::Not,
    sync::{atomic::Ordering, Arc},
};

use dashmap::{mapref::entry::Entry as DashMapEntry, DashMap};
use mirrord_protocol::{Port, RemoteResult, ResponseError};

use super::http::HttpFilter;
use crate::{
    incoming::{RedirectedConnection, RedirectorTaskError, StealHandle},
    metrics::{STEAL_FILTERED_PORT_SUBSCRIPTION, STEAL_UNFILTERED_PORT_SUBSCRIPTION},
    util::ClientId,
};

/// Set of active port subscriptions.
pub struct PortSubscriptions {
    /// Used to request port redirections and fetch redirected connections.
    handle: StealHandle,
    /// Maps ports to active subscriptions.
    subscriptions: HashMap<Port, PortSubscription>,
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

impl PortSubscriptions {
    /// Create an empty instance of this struct.
    ///
    /// # Params
    ///
    /// * `handle` - will be used to enforce connection stealing according to the state of this set.
    /// * `initial_capacity` - initial capacity for the inner (port -> subscription) mapping.
    pub fn new(handle: StealHandle, initial_capacity: usize) -> Self {
        Self {
            handle,
            subscriptions: HashMap::with_capacity(initial_capacity),
        }
    }

    /// Try adding a new subscription to this set.
    ///
    /// # Subscription clash rules
    ///
    /// * A single client may have only one subscription for the given port
    /// * A single port may have only one unfiltered subscription
    ///
    /// # Params
    ///
    /// * `client_id` - identifier of the client that issued the subscription
    /// * `port` - number of the port to steal from
    /// * `client_protocol_version` - version of client's [`mirrord_protocol`], [`None`] if the
    ///   version has not been negotiated yet
    /// * `filter` - optional [`HttpFilter`]
    pub async fn add(
        &mut self,
        client_id: ClientId,
        port: Port,
        client_protocol_version: Option<semver::Version>,
        filter: Option<HttpFilter>,
    ) -> Result<RemoteResult<Port>, RedirectorTaskError> {
        let filtered = filter.is_some();

        match self.subscriptions.entry(port) {
            Entry::Occupied(mut e) => {
                if e.get_mut()
                    .try_extend(client_id, client_protocol_version, filter)
                    .not()
                {
                    return Ok(Err(ResponseError::PortAlreadyStolen(port)));
                }
            }

            Entry::Vacant(e) => {
                self.handle.steal(port).await?;

                e.insert(PortSubscription::new(
                    client_id,
                    client_protocol_version,
                    filter,
                ));
            }
        };

        if filtered {
            STEAL_FILTERED_PORT_SUBSCRIPTION.fetch_add(1, Ordering::Relaxed);
        } else {
            STEAL_UNFILTERED_PORT_SUBSCRIPTION.fetch_add(1, Ordering::Relaxed);
        }

        Ok(Ok(port))
    }

    /// Remove a subscription from this set, if it exists.
    ///
    /// # Params
    ///
    /// * `client_id` - identifier of the client that issued the subscription
    /// * `port` - number of the subscription port
    pub fn remove(&mut self, client_id: ClientId, port: Port) {
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

    /// Wait until there's a new redirected connection.
    pub async fn next_connection(
        &mut self,
    ) -> Result<(RedirectedConnection, PortSubscription), RedirectorTaskError> {
        loop {
            let conn = self.handle.next().await.transpose()?;

            let Some(conn) = conn else {
                break std::future::pending().await;
            };

            if let Some(subscription) = self.subscriptions.get(&conn.destination().port()) {
                break Ok((conn, subscription.clone()));
            }
        }
    }
}

/// Maps id of the client to their active [`HttpFilter`] and their negotiated [`mirrord_protocol`]
/// version, if any.
///
/// [`mirrord_protocol`] version affects which stolen requests can be handled by the client.
pub type Filters = Arc<DashMap<ClientId, (HttpFilter, Option<semver::Version>)>>;

/// Steal subscription for a port.
#[derive(Debug, Clone)]
pub enum PortSubscription {
    /// No filter, incoming connections are stolen whole on behalf of the client.
    ///
    /// Belongs to a single client.
    Unfiltered(ClientId),
    /// Only HTTP requests matching one of the [`HttpFilter`]s should be stolen (on behalf of the
    /// filter owner).
    ///
    /// Can be shared by multiple clients.
    Filtered(Filters),
}

impl PortSubscription {
    /// Create a new instance. Variant is picked based on the optional `filter`.
    fn new(
        client_id: ClientId,
        client_protocol_version: Option<semver::Version>,
        filter: Option<HttpFilter>,
    ) -> Self {
        match filter {
            Some(filter) => Self::Filtered(Arc::new(DashMap::from_iter([(
                client_id,
                (filter, client_protocol_version),
            )]))),
            None => Self::Unfiltered(client_id),
        }
    }

    /// Try extending this subscription with a new subscription request.
    /// Return whether extension was successful.
    fn try_extend(
        &mut self,
        client_id: ClientId,
        client_protocol_version: Option<semver::Version>,
        filter: Option<HttpFilter>,
    ) -> bool {
        match (self, filter) {
            (_, None) => false,

            (Self::Unfiltered(..), _) => false,

            (Self::Filtered(filters), Some(filter)) => match filters.entry(client_id) {
                DashMapEntry::Occupied(..) => false,
                DashMapEntry::Vacant(e) => {
                    e.insert((filter, client_protocol_version));
                    true
                }
            },
        }
    }
}

#[cfg(test)]
mod test {
    use mirrord_protocol::ResponseError;

    use crate::{
        incoming::{test::DummyRedirector, RedirectorTask},
        steal::{
            http::HttpFilter,
            subscriptions::{PortSubscription, PortSubscriptions},
        },
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
        let (redirector_task, steal_handle) = RedirectorTask::new(redirector);
        tokio::spawn(redirector_task.run());
        let mut subscriptions = PortSubscriptions::new(steal_handle, 8);

        // Adding unfiltered subscription.
        subscriptions.add(0, 80, None, None).await.unwrap().unwrap();
        assert!(state.borrow().has_redirections([80]));
        let sub = subscriptions.subscriptions.get(&80).unwrap();
        assert!(matches!(sub, PortSubscription::Unfiltered(0)), "{sub:?}");

        // Same client cannot subscribe again (unfiltered).
        assert_eq!(
            subscriptions.add(0, 80, None, None).await.unwrap(),
            Err(ResponseError::PortAlreadyStolen(80)),
        );
        assert!(state.borrow().has_redirections([80]));
        let sub = subscriptions.subscriptions.get(&80).unwrap();
        assert!(matches!(sub, PortSubscription::Unfiltered(0)), "{sub:?}");

        // Same client cannot subscribe again (filtered).
        assert_eq!(
            subscriptions
                .add(0, 80, None, Some(dummy_filter()))
                .await
                .unwrap(),
            Err(ResponseError::PortAlreadyStolen(80)),
        );
        assert!(state.borrow().has_redirections([80]));
        let sub = subscriptions.subscriptions.get(&80).unwrap();
        assert!(matches!(sub, PortSubscription::Unfiltered(0)), "{sub:?}");

        // Another client cannot subscribe (unfiltered).
        assert_eq!(
            subscriptions.add(1, 80, None, None).await.unwrap(),
            Err(ResponseError::PortAlreadyStolen(80)),
        );
        assert!(state.borrow().has_redirections([80]));
        let sub = subscriptions.subscriptions.get(&80).unwrap();
        assert!(matches!(sub, PortSubscription::Unfiltered(0)), "{sub:?}");

        // Another client cannot subscribe (filtered).
        assert_eq!(
            subscriptions
                .add(1, 80, None, Some(dummy_filter()))
                .await
                .unwrap(),
            Err(ResponseError::PortAlreadyStolen(80)),
        );
        assert!(state.borrow().has_redirections([80]));
        let sub = subscriptions.subscriptions.get(&80).unwrap();
        assert!(matches!(sub, PortSubscription::Unfiltered(0)), "{sub:?}");

        // Removing unfiltered subscription.
        subscriptions.remove(0, 80);

        // Checking if all is cleaned up.
        state
            .wait_for(|state| state.has_redirections([]))
            .await
            .unwrap();
        let sub = subscriptions.subscriptions.get(&80);
        assert!(sub.is_none(), "{sub:?}");

        // Adding filtered subscription.
        subscriptions
            .add(0, 80, None, Some(dummy_filter()))
            .await
            .unwrap()
            .unwrap();
        assert!(state.borrow().has_redirections([80]));
        let sub = subscriptions.subscriptions.get(&80).unwrap();
        assert!(
            matches!(sub, PortSubscription::Filtered(filters) if filters.len() == 1),
            "{sub:?}"
        );

        // Same client cannot subscribe again (unfiltered).
        assert_eq!(
            subscriptions.add(0, 80, None, None).await.unwrap(),
            Err(ResponseError::PortAlreadyStolen(80)),
        );
        assert!(state.borrow().has_redirections([80]));
        let sub = subscriptions.subscriptions.get(&80).unwrap();
        assert!(
            matches!(sub, PortSubscription::Filtered(filters) if filters.len() == 1),
            "{sub:?}"
        );

        // Same client cannot subscribe again (filtered).
        assert_eq!(
            subscriptions
                .add(0, 80, None, Some(dummy_filter()))
                .await
                .unwrap(),
            Err(ResponseError::PortAlreadyStolen(80)),
        );
        assert!(state.borrow().has_redirections([80]));
        let sub = subscriptions.subscriptions.get(&80).unwrap();
        assert!(
            matches!(sub, PortSubscription::Filtered(filters) if filters.len() == 1),
            "{sub:?}"
        );

        // Another client cannot subscribe (unfiltered).
        assert_eq!(
            subscriptions.add(1, 80, None, None).await.unwrap(),
            Err(ResponseError::PortAlreadyStolen(80)),
        );
        assert!(state.borrow().has_redirections([80]));
        let sub = subscriptions.subscriptions.get(&80).unwrap();
        assert!(
            matches!(sub, PortSubscription::Filtered(filters) if filters.len() == 1),
            "{sub:?}"
        );

        // Another client can subscribe (filtered).
        subscriptions
            .add(1, 80, None, Some(dummy_filter()))
            .await
            .unwrap()
            .unwrap();
        assert!(state.borrow().has_redirections([80]));
        let sub = subscriptions.subscriptions.get(&80).unwrap();
        assert!(
            matches!(sub, PortSubscription::Filtered(filters) if filters.len() == 2),
            "{sub:?}"
        );

        // Removing first subscription.
        subscriptions.remove(0, 80);

        // Checking if the second subscription still exists.
        let sub = subscriptions.subscriptions.get(&80).unwrap();
        assert!(
            matches!(sub, PortSubscription::Filtered(filters) if filters.len() == 1),
            "{sub:?}"
        );

        // Removing second subscription.
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
        let (redirector_task, steal_handle) = RedirectorTask::new(redirector);
        tokio::spawn(redirector_task.run());
        let mut subscriptions = PortSubscriptions::new(steal_handle, 8);

        // Adding unfiltered subscription for port 80.
        subscriptions.add(0, 80, None, None).await.unwrap().unwrap();

        // Adding filtered subscription for port 81.
        subscriptions
            .add(1, 81, None, Some(dummy_filter()))
            .await
            .unwrap()
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
        // check_redirector!(subscriptions.redirector);
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
        let (redirector_task, steal_handle) = RedirectorTask::new(redirector);
        tokio::spawn(redirector_task.run());
        let mut subscriptions = PortSubscriptions::new(steal_handle, 8);

        // Adding unfiltered subscription for port 80.
        subscriptions.add(0, 80, None, None).await.unwrap().unwrap();

        // Adding filtered subscription for port 81.
        subscriptions
            .add(0, 81, None, Some(dummy_filter()))
            .await
            .unwrap()
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
