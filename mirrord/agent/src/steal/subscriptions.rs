use std::{
    collections::{hash_map::Entry, HashMap},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    ops::Not,
    sync::Arc,
};

use dashmap::{mapref::entry::Entry as DashMapEntry, DashMap};
use mirrord_protocol::{Port, RemoteResult, ResponseError};
use tokio::{
    net::{TcpListener, TcpStream},
    select,
};

use super::{
    http::HttpFilter,
    ip_tables::{new_iptables, IPTablesWrapper, SafeIpTables},
};
use crate::{error::AgentError, util::ClientId};

/// For stealing incoming TCP connections.
#[async_trait::async_trait]
pub trait PortRedirector {
    type Error;

    /// Start stealing connections from the given port.
    ///
    /// # Note
    ///
    /// If a redirection from the given port already exists, implementations are free to do nothing
    /// or return an [`Err`].
    async fn add_redirection(&mut self, from: Port) -> Result<(), Self::Error>;

    /// Stop stealing connections from the given port.
    ///
    /// # Note
    ///
    /// If the redirection does no exist, implementations are free to do nothing or return an
    /// [`Err`].
    async fn remove_redirection(&mut self, from: Port) -> Result<(), Self::Error>;

    /// Clean any external state.
    async fn cleanup(&mut self) -> Result<(), Self::Error>;

    /// Accept an incoming redirected connection.
    ///
    /// # Returns
    ///
    /// * [`TcpStream`] - redirected connection
    /// * [`SocketAddr`] - peer address
    async fn next_connection(&mut self) -> Result<(TcpStream, SocketAddr), Self::Error>;
}

/// A TCP listener, together with an iptables wrapper to set rules that send traffic to the
/// listener.
pub(crate) struct IptablesListener {
    /// For altering iptables rules.
    iptables: Option<SafeIpTables<IPTablesWrapper>>,
    /// Port of [`IpTablesRedirector::listener`].
    redirect_to: Port,
    /// Listener to which redirect all connections.
    listener: TcpListener,
    /// Optional comma-seperated list of IPs of the pod, originating in the pod's `Status.PodIps`
    pod_ips: Option<String>,
    /// Whether existing connections should be flushed when adding new redirects.
    flush_connections: bool,
}

#[async_trait::async_trait]
impl PortRedirector for IptablesListener {
    type Error = AgentError;
    async fn add_redirection(&mut self, from: Port) -> Result<(), Self::Error> {
        let iptables = if let Some(iptables) = self.iptables.as_ref() {
            iptables
        } else {
            let safe = crate::steal::ip_tables::SafeIpTables::create(
                new_iptables().into(),
                self.flush_connections,
                self.pod_ips.as_deref(),
            )
            .await?;
            self.iptables.insert(safe)
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

    async fn next_connection(&mut self) -> Result<(TcpStream, SocketAddr), Self::Error> {
        self.listener.accept().await.map_err(Into::into)
    }
}

/// Implementation of [`PortRedirector`] that manipulates iptables to steal connections by
/// redirecting TCP packets to inner [`TcpListener`].
///
/// Holds TCP listeners + iptables, for redirecting IPv4 and/or IPv6 connections.
pub(crate) enum IpTablesRedirector {
    Ipv4Only(IptablesListener),
    /// Could be used if IPv6 support is enabled, and we cannot bind an IPv4 address.
    Ipv6Only(IptablesListener),
    Dual {
        ipv4_listener: IptablesListener,
        ipv6_listener: IptablesListener,
    },
}

impl IpTablesRedirector {
    /// Create a new instance of this struct. Open an IPv4 TCP listener on an
    /// [`Ipv4Addr::UNSPECIFIED`] address and a random port. This listener will be used to accept
    /// redirected connections.
    ///
    /// If `support_ipv6` is set, will also listen on IPv6, and a fail to listen over IPv4 will be
    /// accepted.
    ///
    /// # Note
    ///
    /// Does not yet alter iptables.
    ///
    /// # Params
    ///
    /// * `flush_connections` - whether existing connections should be flushed when adding new
    ///   redirects
    pub(crate) async fn new(
        flush_connections: bool,
        pod_ips: Option<String>,
        support_ipv6: bool,
    ) -> Result<Self, AgentError> {
        let (pod_ips4, pod_ips6) = pod_ips.map_or_else(
            || (None, None),
            |ips| {
                // TODO: probably nicer to split at the client and avoid the conversion to and back
                //   from a string.
                let (ip4s, ip6s): (Vec<_>, Vec<_>) = ips.split(',').partition(|ip_str| {
                    ip_str
                        .parse::<IpAddr>()
                        .inspect_err(|e| tracing::warn!(%e, "failed to parse pod IP {ip_str}"))
                        .as_ref()
                        .map(IpAddr::is_ipv4)
                        .unwrap_or_default()
                });
                // Convert to options, `None` if vector is empty.
                (
                    ip4s.is_empty().not().then(|| ip4s.join(",")),
                    ip6s.is_empty().not().then(|| ip6s.join(",")),
                )
            },
        );

        let listener4 = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await
                .inspect_err(
                    |err| tracing::debug!(%err, "Could not bind IPv4, continuing with IPv6 only."),
                )
                .ok()
                .and_then(|listener| {
                    let redirect_to = listener
                        .local_addr()
                        .inspect_err(
                            |err| tracing::debug!(%err, "Get IPv4 listener address, continuing with IPv6 only."),
                        )
                        .ok()?
                        .port();
                    Some(IptablesListener {
                        iptables: None,
                        redirect_to,
                        listener,
                        pod_ips: pod_ips4,
                        flush_connections,
                    })
                });
        let listener6 = if support_ipv6 {
            TcpListener::bind((Ipv6Addr::UNSPECIFIED, 0)).await
                    .inspect_err(
                        |err| tracing::debug!(%err, "Could not bind IPv6, continuing with IPv4 only."),
                    )
                    .ok()
                    .and_then(|listener| {
                        let redirect_to = listener
                            .local_addr()
                            .inspect_err(
                                |err| tracing::debug!(%err, "Get IPv6 listener address, continuing with IPv4 only."),
                            )
                            .ok()?
                            .port();
                        Some(IptablesListener {
                            iptables: None,
                            redirect_to,
                            listener,
                            pod_ips: pod_ips6,
                            flush_connections,
                        })
                    })
        } else {
            None
        };
        match (listener4, listener6) {
            (None, None) => Err(AgentError::CannotListenForStolenConnections),
            (Some(ipv4_listener), None) => Ok(Self::Ipv4Only(ipv4_listener)),
            (None, Some(ipv6_listener)) => Ok(Self::Ipv6Only(ipv6_listener)),
            (Some(ipv4_listener), Some(ipv6_listener)) => Ok(Self::Dual {
                ipv4_listener,
                ipv6_listener,
            }),
        }
    }

    pub(crate) fn get_ipv4_listener_mut(&mut self) -> Option<&mut IptablesListener> {
        match self {
            IpTablesRedirector::Ipv6Only(_) => None,
            IpTablesRedirector::Dual { ipv4_listener, .. }
            | IpTablesRedirector::Ipv4Only(ipv4_listener) => Some(ipv4_listener),
        }
    }

    pub(crate) fn get_ipv6_listener_mut(&mut self) -> Option<&mut IptablesListener> {
        match self {
            IpTablesRedirector::Ipv4Only(_) => None,
            IpTablesRedirector::Dual { ipv6_listener, .. }
            | IpTablesRedirector::Ipv6Only(ipv6_listener) => Some(ipv6_listener),
        }
    }

    pub(crate) fn get_listeners_mut(
        &mut self,
    ) -> (Option<&mut IptablesListener>, Option<&mut IptablesListener>) {
        match self {
            IpTablesRedirector::Ipv4Only(ipv4_listener) => (Some(ipv4_listener), None),
            IpTablesRedirector::Ipv6Only(ipv6_listener) => (None, Some(ipv6_listener)),
            IpTablesRedirector::Dual {
                ipv4_listener,
                ipv6_listener,
            } => (Some(ipv4_listener), Some(ipv6_listener)),
        }
    }
}

#[async_trait::async_trait]
impl PortRedirector for IpTablesRedirector {
    type Error = AgentError;

    async fn add_redirection(&mut self, from: Port) -> Result<(), Self::Error> {
        let (ipv4_listener, ipv6_listener) = self.get_listeners_mut();
        if let Some(ip4_listener) = ipv4_listener {
            ip4_listener.add_redirection(from).await?;
        }
        if let Some(ip6_listener) = ipv6_listener {
            ip6_listener.add_redirection(from).await?;
        }
        Ok(())
    }

    async fn remove_redirection(&mut self, from: Port) -> Result<(), Self::Error> {
        let (ipv4_listener, ipv6_listener) = self.get_listeners_mut();
        if let Some(ip4_listener) = ipv4_listener {
            ip4_listener.remove_redirection(from).await?;
        }
        if let Some(ip6_listener) = ipv6_listener {
            ip6_listener.remove_redirection(from).await?;
        }
        Ok(())
    }

    async fn cleanup(&mut self) -> Result<(), Self::Error> {
        let (ipv4_listener, ipv6_listener) = self.get_listeners_mut();
        if let Some(ip4_listener) = ipv4_listener {
            ip4_listener.cleanup().await?;
        }
        if let Some(ip6_listener) = ipv6_listener {
            ip6_listener.cleanup().await?;
        }
        Ok(())
    }

    async fn next_connection(&mut self) -> Result<(TcpStream, SocketAddr), Self::Error> {
        match self {
            Self::Dual {
                ipv4_listener,
                ipv6_listener,
            } => {
                select! {
                    con = ipv4_listener.next_connection() => con,
                    con = ipv6_listener.next_connection() => con,
                }
            }
            Self::Ipv4Only(listener) | Self::Ipv6Only(listener) => listener.next_connection().await,
        }
    }
}

/// Set of active port subscriptions.
pub struct PortSubscriptions<R: PortRedirector> {
    /// Used to implement stealing connections.
    redirector: R,
    /// Maps ports to active subscriptions.
    subscriptions: HashMap<Port, PortSubscription>,
}

impl<R: PortRedirector> PortSubscriptions<R> {
    /// Create an empty instance of this struct.
    ///
    /// # Params
    ///
    /// * `redirector` - will be used to enforce connection stealing according to the state of this
    ///   set
    /// * `initial_capacity` - initial capacity for the inner (port -> subscription) mapping
    pub fn new(redirector: R, initial_capacity: usize) -> Self {
        Self {
            redirector,
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
    /// * `filter` - optional [`HttpFilter`]
    ///
    /// # Warning
    ///
    /// If this method returns an [`Err`], it means that this set is out of sync with the inner
    /// [`PortRedirector`] and it is no longer usable. It is a caller's responsibility to clean
    /// up any external state.
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

    /// Remove a subscription from this set, if it exists.
    ///
    /// # Params
    ///
    /// * `client_id` - identifier of the client that issued the subscription
    /// * `port` - number of the subscription port
    ///
    /// # Warning
    ///
    /// If this method returns an [`Err`], it means that this set is out of sync with the inner
    /// [`PortRedirector`] and it is no longer usable. It is a caller's responsibility to clean
    /// up any external state.
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

    /// Remove all client subscriptions from this set.
    ///
    /// # Params
    ///
    /// * `client_id` - identifier of the client that issued the subscriptions
    ///
    /// # Warning
    ///
    /// If this method returns an [`Err`], it means that this set is out of sync with the inner
    /// [`PortRedirector`] and it is no longer usable. It is a caller's responsibility to clean
    /// up any external state.
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

    /// Return a subscription for the given `port`.
    pub fn get(&self, port: Port) -> Option<&PortSubscription> {
        self.subscriptions.get(&port)
    }

    /// Call [`PortRedirector::next_connection`] on the inner [`PortRedirector`].
    pub async fn next_connection(&mut self) -> Result<(TcpStream, SocketAddr), R::Error> {
        self.redirector.next_connection().await
    }
}

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
    Filtered(Arc<DashMap<ClientId, HttpFilter>>),
}

impl PortSubscription {
    /// Create a new instance. Variant is picked based on the optional `filter`.
    fn new(client_id: ClientId, filter: Option<HttpFilter>) -> Self {
        match filter {
            Some(filter) => Self::Filtered(Arc::new([(client_id, filter)].into_iter().collect())),
            None => Self::Unfiltered(client_id),
        }
    }

    /// Try extending this subscription with a new subscription request.
    /// Return whether extension was successful.
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

    /// Return whether this subscription belongs (possibly partially) to the given client.
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

    /// Implementation of [`PortRedirector`] that stores redirections in memory.
    /// Disallows duplicate redirections or removing a non-existent redirection.
    #[derive(Default)]
    struct DummyRedirector {
        redirections: HashSet<Port>,
        dirty: bool,
    }

    /// Checks the redirections in the given [`DummyRedirector`] against a sequence of ports.
    ///
    /// # Usage
    ///
    /// * To assert exact set of redirections: `check_redirector!(redirector, 80, 81, 3000)`
    /// * To assert no redirections: `check_redirector!(redirector)`
    ///
    /// # Note
    ///
    /// It's implemented as a macro only to preserve the original line number should the test fail.
    macro_rules! check_redirector {
        ( $redirector: expr $(, $x:expr )* ) => {
            {
                let mut temp_vec: Vec<u16> = vec![$($x,)*];
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

        async fn next_connection(&mut self) -> Result<(TcpStream, SocketAddr), Self::Error> {
            unimplemented!()
        }
    }

    fn dummy_filter() -> HttpFilter {
        HttpFilter::Header(".*".parse().unwrap())
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
        assert!(sub.is_none(), "{sub:?}");

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
        assert!(sub.is_none(), "{sub:?}");
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
        assert!(sub.is_none(), "{sub:?}");
        let sub = subscriptions.get(81);
        assert!(sub.is_none(), "{sub:?}");
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
        assert!(sub.is_none(), "{sub:?}");
        let sub = subscriptions.get(81);
        assert!(sub.is_none(), "{sub:?}");
    }
}
