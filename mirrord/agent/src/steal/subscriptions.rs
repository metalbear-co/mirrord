use std::{
    collections::{hash_map::Entry, HashMap},
    sync::Arc,
};

use dashmap::{mapref::entry::Entry as DashMapEntry, DashMap};
use mirrord_protocol::{Port, RemoteResult, ResponseError};

use super::{
    http::HttpFilter,
    ip_tables::{IPTablesWrapper, SafeIpTables},
};
use crate::{error::AgentError, util::ClientId};

#[async_trait::async_trait]
pub trait PortRedirector {
    type Error;

    async fn add_redirection(&mut self, from: Port) -> Result<(), Self::Error>;

    async fn remove_redirection(&mut self, from: Port) -> Result<(), Self::Error>;

    async fn cleanup(&mut self) -> Result<(), Self::Error>;
}

pub(crate) struct IpTablesRedirector {
    iptables: Option<SafeIpTables<IPTablesWrapper>>,
    flush_connections: bool,
    redirect_to: Port,
}

impl IpTablesRedirector {
    pub(crate) fn new(redirect_to: Port, flush_connections: bool) -> Self {
        Self {
            iptables: None,
            flush_connections,
            redirect_to,
        }
    }
}

#[async_trait::async_trait]
impl PortRedirector for IpTablesRedirector {
    type Error = AgentError;

    async fn add_redirection(&mut self, from: Port) -> Result<(), Self::Error> {
        let iptables = match self.iptables.as_ref() {
            Some(iptables) => iptables,
            None => {
                let iptables = iptables::new(false).unwrap();
                let safe = SafeIpTables::create(iptables.into(), self.flush_connections).await?;
                self.iptables.insert(safe)
            }
        };

        iptables.add_redirect(from, self.redirect_to).await
    }

    async fn remove_redirection(&mut self, from: Port) -> Result<(), Self::Error> {
        if let Some(iptables) = self.iptables.as_ref() {
            iptables.remove_redirect(from, self.redirect_to).await?;
        }

        Ok(())
    }

    async fn cleanup(&mut self) -> Result<(), Self::Error> {
        if let Some(iptables) = self.iptables.take() {
            iptables.cleanup().await?;
        }

        Ok(())
    }
}

pub struct PortSubscriptions<R: PortRedirector> {
    redirector: R,
    subscriptions: HashMap<Port, PortSubscription>,
}

impl<R: PortRedirector> PortSubscriptions<R> {
    pub fn new(redirector: R, initial_capacity: usize) -> Self {
        Self {
            redirector,
            subscriptions: HashMap::with_capacity(initial_capacity),
        }
    }

    pub async fn add(
        &mut self,
        client_id: ClientId,
        port: Port,
        filter: Option<HttpFilter>,
    ) -> Result<RemoteResult<Port>, R::Error> {
        let add_redirect = match self.subscriptions.entry(port) {
            Entry::Occupied(mut e) => {
                if e.get_mut().try_extend(client_id, filter) {
                    Ok(false)
                } else {
                    Err(ResponseError::PortAlreadyStolen(port))
                }
            }

            Entry::Vacant(e) => {
                e.insert(PortSubscription::new(client_id, filter));
                Ok(true)
            }
        };

        match add_redirect {
            Ok(true) => {
                self.redirector.add_redirection(port).await?;

                Ok(Ok(port))
            }
            Ok(false) => Ok(Ok(port)),
            Err(e) => Ok(Err(e)),
        }
    }

    pub async fn remove(&mut self, client_id: ClientId, port: Port) -> Result<(), R::Error> {
        let Entry::Occupied(mut e) = self.subscriptions.entry(port) else {
            return Ok(());
        };

        let remove_redirect = match e.get_mut() {
            PortSubscription::Unfiltered(subscribed_client) if *subscribed_client == client_id => {
                e.remove();
                true
            }
            PortSubscription::Unfiltered(..) => false,
            PortSubscription::Filtered(filters) => {
                filters.remove(&client_id);

                if filters.is_empty() {
                    e.remove();
                    true
                } else {
                    false
                }
            }
        };

        if remove_redirect {
            self.redirector.remove_redirection(port).await?;

            if self.subscriptions.is_empty() {
                self.redirector.cleanup().await?;
            }
        }

        Ok(())
    }

    pub async fn remove_all(&mut self, client_id: ClientId) -> Result<(), R::Error> {
        let ports = self
            .subscriptions
            .iter()
            .filter_map(|(k, v)| v.has_client(client_id).then_some(*k))
            .collect::<Vec<_>>();

        for port in ports {
            self.remove(client_id, port).await?;
        }

        Ok(())
    }

    pub fn get(&self, port: Port) -> Option<&PortSubscription> {
        self.subscriptions.get(&port)
    }
}

#[derive(Debug)]
pub enum PortSubscription {
    Unfiltered(ClientId),
    Filtered(Arc<DashMap<ClientId, HttpFilter>>),
}

impl PortSubscription {
    fn new(client_id: ClientId, filter: Option<HttpFilter>) -> Self {
        match filter {
            Some(filter) => Self::Filtered(Arc::new([(client_id, filter)].into_iter().collect())),
            None => Self::Unfiltered(client_id),
        }
    }

    fn try_extend(&mut self, client_id: ClientId, filter: Option<HttpFilter>) -> bool {
        match (self, filter) {
            (_, None) => false,

            (Self::Unfiltered(..), _) => false,

            (Self::Filtered(filters), Some(filter)) => match filters.entry(client_id) {
                DashMapEntry::Occupied(..) => false,
                DashMapEntry::Vacant(e) => {
                    e.insert(filter);
                    true
                }
            },
        }
    }

    fn has_client(&self, client_id: ClientId) -> bool {
        match self {
            Self::Filtered(filters) => filters.contains_key(&client_id),
            Self::Unfiltered(subscribed_client) => *subscribed_client == client_id,
        }
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashSet;

    use super::*;

    #[derive(Default)]
    struct DummyRedirector {
        redirections: HashSet<Port>,
        dirty: bool,
    }

    macro_rules! check_redirector {
        ( $redirector: expr $(, $x:expr )* ) => {
            {
                let mut temp_vec = Vec::<Port>::new();
                $(
                    temp_vec.push($x);
                )*

                temp_vec.sort();

                let mut redirections = $redirector.redirections.iter().copied().collect::<Vec<_>>();
                redirections.sort();

                assert_eq!(redirections, temp_vec, "redirector in bad state");
            }
        };
    }

    #[async_trait::async_trait]
    impl PortRedirector for DummyRedirector {
        type Error = Port;

        async fn add_redirection(&mut self, from: Port) -> Result<(), Self::Error> {
            if self.redirections.insert(from) {
                self.dirty = true;
                Ok(())
            } else {
                Err(from)
            }
        }

        async fn remove_redirection(&mut self, from: Port) -> Result<(), Self::Error> {
            if self.redirections.remove(&from) {
                Ok(())
            } else {
                Err(from)
            }
        }

        async fn cleanup(&mut self) -> Result<(), Self::Error> {
            self.redirections.clear();
            self.dirty = false;

            Ok(())
        }
    }

    fn dummy_filter() -> HttpFilter {
        HttpFilter::new_header_filter(".*".parse().unwrap())
    }

    #[tokio::test]
    async fn multiple_subscriptions_one_port() {
        let redirector = DummyRedirector::default();
        let mut subscriptions = PortSubscriptions::new(redirector, 8);
        check_redirector!(subscriptions.redirector);

        // Adding unfiltered subscription.
        subscriptions.add(0, 80, None).await.unwrap().unwrap();
        check_redirector!(subscriptions.redirector, 80);
        let sub = subscriptions.get(80).unwrap();
        assert!(matches!(sub, PortSubscription::Unfiltered(0)), "{sub:?}");

        // Same client cannot subscribe again (unfiltered).
        assert_eq!(
            subscriptions.add(0, 80, None).await.unwrap(),
            Err(ResponseError::PortAlreadyStolen(80)),
        );
        check_redirector!(subscriptions.redirector, 80);
        let sub = subscriptions.get(80).unwrap();
        assert!(matches!(sub, PortSubscription::Unfiltered(0)), "{sub:?}");

        // Same client cannot subscribe again (filtered).
        assert_eq!(
            subscriptions
                .add(0, 80, Some(dummy_filter()))
                .await
                .unwrap(),
            Err(ResponseError::PortAlreadyStolen(80)),
        );
        check_redirector!(subscriptions.redirector, 80);
        let sub = subscriptions.get(80).unwrap();
        assert!(matches!(sub, PortSubscription::Unfiltered(0)), "{sub:?}");

        // Another client cannot subscribe (unfiltered).
        assert_eq!(
            subscriptions.add(1, 80, None).await.unwrap(),
            Err(ResponseError::PortAlreadyStolen(80)),
        );
        check_redirector!(subscriptions.redirector, 80);
        let sub = subscriptions.get(80).unwrap();
        assert!(matches!(sub, PortSubscription::Unfiltered(0)), "{sub:?}");

        // Another client cannot subscribe (filtered).
        assert_eq!(
            subscriptions
                .add(1, 80, Some(dummy_filter()))
                .await
                .unwrap(),
            Err(ResponseError::PortAlreadyStolen(80)),
        );
        check_redirector!(subscriptions.redirector, 80);
        let sub = subscriptions.get(80).unwrap();
        assert!(matches!(sub, PortSubscription::Unfiltered(0)), "{sub:?}");

        // Removing unfiltered subscription.
        subscriptions.remove(0, 80).await.unwrap();

        // Checking if all is cleaned up.
        check_redirector!(subscriptions.redirector);
        assert!(!subscriptions.redirector.dirty);
        let sub = subscriptions.get(80);
        assert!(matches!(sub, None), "{sub:?}");

        // Adding filtered subscription.
        subscriptions
            .add(0, 80, Some(dummy_filter()))
            .await
            .unwrap()
            .unwrap();
        check_redirector!(subscriptions.redirector, 80);
        let sub = subscriptions.get(80).unwrap();
        assert!(
            matches!(sub, PortSubscription::Filtered(filters) if filters.len() == 1),
            "{sub:?}"
        );

        // Same client cannot subscribe again (unfiltered).
        assert_eq!(
            subscriptions.add(0, 80, None).await.unwrap(),
            Err(ResponseError::PortAlreadyStolen(80)),
        );
        check_redirector!(subscriptions.redirector, 80);
        let sub = subscriptions.get(80).unwrap();
        assert!(
            matches!(sub, PortSubscription::Filtered(filters) if filters.len() == 1),
            "{sub:?}"
        );

        // Same client cannot subscribe again (filtered).
        assert_eq!(
            subscriptions
                .add(0, 80, Some(dummy_filter()))
                .await
                .unwrap(),
            Err(ResponseError::PortAlreadyStolen(80)),
        );
        check_redirector!(subscriptions.redirector, 80);
        let sub = subscriptions.get(80).unwrap();
        assert!(
            matches!(sub, PortSubscription::Filtered(filters) if filters.len() == 1),
            "{sub:?}"
        );

        // Another client cannot subscribe (unfiltered).
        assert_eq!(
            subscriptions.add(1, 80, None).await.unwrap(),
            Err(ResponseError::PortAlreadyStolen(80)),
        );
        check_redirector!(subscriptions.redirector, 80);
        let sub = subscriptions.get(80).unwrap();
        assert!(
            matches!(sub, PortSubscription::Filtered(filters) if filters.len() == 1),
            "{sub:?}"
        );

        // Another client can subscribe (filtered).
        subscriptions
            .add(1, 80, Some(dummy_filter()))
            .await
            .unwrap()
            .unwrap();
        check_redirector!(subscriptions.redirector, 80);
        let sub = subscriptions.get(80).unwrap();
        assert!(
            matches!(sub, PortSubscription::Filtered(filters) if filters.len() == 2),
            "{sub:?}"
        );

        // Removing first subscription.
        subscriptions.remove(0, 80).await.unwrap();

        // Checking if the second subscription still exists.
        check_redirector!(subscriptions.redirector, 80);
        let sub = subscriptions.get(80).unwrap();
        assert!(
            matches!(sub, PortSubscription::Filtered(filters) if filters.len() == 1),
            "{sub:?}"
        );

        // Removing second subscription.
        subscriptions.remove(1, 80).await.unwrap();

        // Checking if all is cleaned up.
        check_redirector!(subscriptions.redirector);
        assert!(!subscriptions.redirector.dirty);
        let sub = subscriptions.get(80);
        assert!(matches!(sub, None), "{sub:?}");
    }

    #[tokio::test]
    async fn multiple_subscriptions_multiple_ports() {
        let redirector = DummyRedirector::default();
        let mut subscriptions = PortSubscriptions::new(redirector, 8);
        check_redirector!(subscriptions.redirector);

        // Adding unfiltered subscription for port 80.
        subscriptions.add(0, 80, None).await.unwrap().unwrap();

        // Adding filtered subscription for port 81.
        subscriptions
            .add(1, 81, Some(dummy_filter()))
            .await
            .unwrap()
            .unwrap();

        // Checking state.
        check_redirector!(subscriptions.redirector, 80, 81);
        let sub = subscriptions.get(80).unwrap();
        assert!(sub.has_client(0));
        assert!(matches!(sub, PortSubscription::Unfiltered(0)), "{sub:?}");
        let sub = subscriptions.get(81).unwrap();
        assert!(sub.has_client(1));
        assert!(
            matches!(sub, PortSubscription::Filtered(filters) if filters.len() == 1),
            "{sub:?}"
        );

        // Removing subscriptions.
        subscriptions.remove(0, 80).await.unwrap();
        subscriptions.remove(1, 81).await.unwrap();

        // Checking if all is cleaned up.
        check_redirector!(subscriptions.redirector);
        assert!(!subscriptions.redirector.dirty);
        let sub = subscriptions.get(80);
        assert!(matches!(sub, None), "{sub:?}");
        let sub = subscriptions.get(81);
        assert!(matches!(sub, None), "{sub:?}");
    }

    #[tokio::test]
    async fn remove_all_from_client() {
        let redirector = DummyRedirector::default();
        let mut subscriptions = PortSubscriptions::new(redirector, 8);
        check_redirector!(subscriptions.redirector);

        // Adding unfiltered subscription for port 80.
        subscriptions.add(0, 80, None).await.unwrap().unwrap();

        // Adding filtered subscription for port 81.
        subscriptions
            .add(0, 81, Some(dummy_filter()))
            .await
            .unwrap()
            .unwrap();

        // Checking state.
        check_redirector!(subscriptions.redirector, 80, 81);
        let sub = subscriptions.get(80).unwrap();
        assert!(sub.has_client(0));
        assert!(matches!(sub, PortSubscription::Unfiltered(0)), "{sub:?}");
        let sub = subscriptions.get(81).unwrap();
        assert!(sub.has_client(0));
        assert!(
            matches!(sub, PortSubscription::Filtered(filters) if filters.len() == 1),
            "{sub:?}"
        );

        // Removing all subscriptions of a client.
        subscriptions.remove_all(0).await.unwrap();

        // Checking if all is cleaned up.
        check_redirector!(subscriptions.redirector);
        assert!(!subscriptions.redirector.dirty);
        let sub = subscriptions.get(80);
        assert!(matches!(sub, None), "{sub:?}");
        let sub = subscriptions.get(81);
        assert!(matches!(sub, None), "{sub:?}");
    }
}
