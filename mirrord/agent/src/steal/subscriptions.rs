use std::collections::{hash_map::Entry, HashMap};

use crate::{
    http::HttpFilter,
    incoming::{RedirectorTaskError, StealHandle, StolenTraffic},
    util::ClientId,
};

/// A set of steal subscriptions from agent clients.
///
/// Uses a [`StealHandle`] to create port redirections based on the subscriptions.
///
/// # Subscription clash rules
///
/// 1. One port may have only one unfiltered subscription.
/// 2. One port may have only one filtered subscription per client.
pub struct PortSubscriptions {
    steal_handle: StealHandle,
    subscriptions: HashMap<u16, PortSubscription>,
}

impl PortSubscriptions {
    pub fn new(steal_handle: StealHandle) -> Self {
        Self {
            steal_handle,
            subscriptions: HashMap::new(),
        }
    }

    /// Returns the next stolen connection/request,
    /// along with the [`PortSubscription`] that stole it.
    pub async fn next(
        &mut self,
    ) -> Option<Result<(StolenTraffic, &PortSubscription), RedirectorTaskError>> {
        let traffic = match self.steal_handle.next().await? {
            Ok(traffic) => traffic,
            Err(error) => return Some(Err(error)),
        };

        let subscription = self
            .subscriptions
            .get(&traffic.destination_port())
            .expect("port subscription not found");

        Some(Ok((traffic, subscription)))
    }

    /// Adds a new client's subscription to this set.
    ///
    /// Any previous conflicting subscriptions will be removed.
    pub async fn subscribe(
        &mut self,
        port: u16,
        client_id: ClientId,
        filter: Option<HttpFilter>,
    ) -> Result<(), RedirectorTaskError> {
        match self.subscriptions.entry(port) {
            Entry::Occupied(mut e) => match (e.get_mut(), filter) {
                (PortSubscription::Filtered(filters), Some(filter)) => {
                    filters.insert(client_id, filter);
                }

                (.., filter) => *e.get_mut() = PortSubscription::new(client_id, filter),
            },

            Entry::Vacant(e) => {
                self.steal_handle.steal(port).await?;
                e.insert(PortSubscription::new(client_id, filter));
            }
        }

        Ok(())
    }

    /// Removes the client's subscription from this set.
    pub fn unsubscribe(&mut self, port: u16, client_id: ClientId) {
        let Entry::Occupied(mut e) = self.subscriptions.entry(port) else {
            return;
        };

        match e.get_mut() {
            PortSubscription::Unfiltered(client) => {
                if *client == client_id {
                    e.remove();
                    self.steal_handle.stop_steal(port);
                }
            }
            PortSubscription::Filtered(filters) => {
                filters.remove(&client_id);
                if filters.is_empty() {
                    e.remove();
                    self.steal_handle.stop_steal(port);
                }
            }
        }
    }

    /// Removes all client's subscriptions from this set.
    pub fn unsubscribe_all(&mut self, client_id: ClientId) {
        self.subscriptions
            .retain(|port, subscription| match subscription {
                PortSubscription::Unfiltered(client) => {
                    if *client == client_id {
                        self.steal_handle.stop_steal(*port);
                        false
                    } else {
                        true
                    }
                }
                PortSubscription::Filtered(filters) => {
                    filters.remove(&client_id);
                    if filters.is_empty() {
                        self.steal_handle.stop_steal(*port);
                        false
                    } else {
                        true
                    }
                }
            });
    }
}

pub enum PortSubscription {
    Unfiltered(ClientId),
    Filtered(HashMap<ClientId, HttpFilter>),
}

impl PortSubscription {
    fn new(client_id: ClientId, filter: Option<HttpFilter>) -> Self {
        match filter {
            Some(filter) => Self::Filtered([(client_id, filter)].into()),
            None => Self::Unfiltered(client_id),
        }
    }
}
