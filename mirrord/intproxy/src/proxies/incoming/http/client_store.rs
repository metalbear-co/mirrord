use std::{
    cmp, fmt,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures::FutureExt;
use hyper::{Uri, Version};
use mirrord_config::feature::network::incoming::https_delivery::{
    HttpsDeliveryProtocol, LocalHttpsDelivery,
};
use mirrord_protocol::tcp::HttpRequestTransportType;
use mirrord_tls_util::UriExt;
use rustls::pki_types::ServerName;
use tokio::{
    net::TcpStream,
    sync::Notify,
    time::{self, Instant},
};
use tracing::Level;

use super::{tls::LocalTlsSetup, HttpSender, LocalHttpClient, LocalHttpError};

/// Idle [`LocalHttpClient`] caches in [`ClientStore`].
struct IdleLocalClient {
    client: LocalHttpClient,
    last_used: Instant,
}

impl fmt::Debug for IdleLocalClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IdleLocalClient")
            .field("client", &self.client)
            .field("idle_for_s", &self.last_used.elapsed().as_secs_f32())
            .finish()
    }
}

/// Cache for unused [`LocalHttpClient`]s.
///
/// [`LocalHttpClient`] that have not been used for some time are dropped in the background by a
/// dedicated [`tokio::task`]. This timeout is configurable.
///
/// # Note on client reuse with different transport protocols
///
/// API of this store allows for having clients that use different transport protocols.
/// Some of the clients may use TCP, some may use TLS.
///
/// When reusing a client, we compare:
/// 1. Destination socket address
/// 2. HTTP [`Version`]
/// 3. Whether the client uses TLS
///
/// We ignore the fact that [`HttpRequestTransportType::Tls::alpn_protocol`] and
/// [`HttpRequestTransportType::Tls::server_name`] might be different.
/// This is because these parameters are only relevant **before** the connection is upgraded to
/// HTTP. Since an idle [`LocalHttpClient`] is ready to send HTTP requests, we assume it's safe to
/// reuse it.
#[derive(Clone)]
pub struct ClientStore {
    clients: Arc<Mutex<Vec<IdleLocalClient>>>,
    tls_setup: Option<Arc<LocalTlsSetup>>,
    /// Used to notify other tasks when there is a new client in the store.
    ///
    /// Make sure to only call [`Notify::notify_waiters`] and [`Notify::notified`] when holding a
    /// lock on [`Self::clients`]. Otherwise you'll have a race condition.
    notify: Arc<Notify>,
}

impl ClientStore {
    /// Creates a new store.
    ///
    /// The store will keep unused clients alive for at least the given time.
    pub fn new_with_timeout(timeout: Duration, https_delivery: LocalHttpsDelivery) -> Self {
        let store = Self {
            clients: Default::default(),
            notify: Default::default(),
            tls_setup: match https_delivery.protocol {
                HttpsDeliveryProtocol::Tcp => None,
                HttpsDeliveryProtocol::Tls => {
                    let server_name = https_delivery.server_name.and_then(|name| {
                        ServerName::try_from(name)
                            .inspect_err(|_| {
                                tracing::error!(
                                    "Invalid server name was specified for the local HTTPS delivery. \
                                    This should be detected during config verification."
                                )
                            })
                            .ok()
                    });

                    Some(Arc::new(LocalTlsSetup::new(
                        https_delivery.trust_roots,
                        https_delivery.server_cert,
                        server_name,
                    )))
                }
            },
        };

        tokio::spawn(cleanup_task(store.clone(), timeout));

        store
    }

    /// Reuses or creates a new [`LocalHttpClient`].
    #[tracing::instrument(level = Level::TRACE, skip(self), ret, err(level = Level::WARN))]
    pub async fn get(
        &self,
        server_addr: SocketAddr,
        version: Version,
        transport: &HttpRequestTransportType,
        request_uri: &Uri,
    ) -> Result<LocalHttpClient, LocalHttpError> {
        let uses_tls =
            matches!(transport, HttpRequestTransportType::Tls { .. }) && self.tls_setup.is_some();

        if let Some(ready) = self
            .wait_for_ready(server_addr, version, uses_tls)
            .now_or_never()
        {
            tracing::trace!(?ready, "Reused an idle client");
            return Ok(ready);
        }

        tokio::select! {
            biased;

            ready = self.wait_for_ready(server_addr, version, uses_tls) => {
                tracing::trace!(?ready, "Reused an idle client");
                Ok(ready)
            },

            result = self.make_client(server_addr, version, transport, request_uri) => result,
        }
    }

    /// Stores an unused [`LocalHttpClient`], so that it can be reused later.
    #[tracing::instrument(level = Level::TRACE, skip(self))]
    pub fn push_idle(&self, client: LocalHttpClient) {
        let mut guard = self
            .clients
            .lock()
            .expect("ClientStore mutex is poisoned, this is a bug");
        guard.push(IdleLocalClient {
            client,
            last_used: Instant::now(),
        });
        self.notify.notify_waiters();
    }

    /// Waits until there is a ready unused client.
    async fn wait_for_ready(
        &self,
        server_addr: SocketAddr,
        version: Version,
        uses_tls: bool,
    ) -> LocalHttpClient {
        loop {
            let notified = {
                let mut guard = self
                    .clients
                    .lock()
                    .expect("ClientStore mutex is poisoned, this is a bug");
                let position = guard.iter().position(|idle| {
                    idle.client.handles_version(version)
                        && idle.client.local_server_address() == server_addr
                        && idle.client.uses_tls() == uses_tls
                });

                match position {
                    Some(position) => return guard.swap_remove(position).client,
                    None => self.notify.notified(),
                }
            };

            notified.await;
        }
    }

    /// Makes an HTTP/HTTPS connection with the given server and creates a new client.
    #[tracing::instrument(level = Level::TRACE, skip(self), err(level = Level::WARN), ret)]
    async fn make_client(
        &self,
        local_server_address: SocketAddr,
        version: Version,
        transport: &HttpRequestTransportType,
        request_uri: &Uri,
    ) -> Result<LocalHttpClient, LocalHttpError> {
        let connector_and_name = match (transport, self.tls_setup.as_ref()) {
            (HttpRequestTransportType::Tcp, ..) => None,
            (.., None) => None,
            (
                HttpRequestTransportType::Tls {
                    alpn_protocol,
                    server_name: original_server_name,
                },
                Some(setup),
            ) => {
                let (connector, server_name) = setup.get(alpn_protocol.clone()).await?;

                let server_name = server_name
                    .or_else(|| {
                        let name = original_server_name.clone()?;
                        ServerName::try_from(name).ok()
                    })
                    .or_else(|| request_uri.get_server_name()?.to_owned().into())
                    .unwrap_or_else(|| {
                        ServerName::try_from("localhost").expect("'localhost' is a valid DNS name")
                    });

                Some((connector, server_name))
            }
        };

        let uses_tls = connector_and_name.is_some();

        let stream = TcpStream::connect(local_server_address)
            .await
            .map_err(LocalHttpError::ConnectTcpFailed)?;
        let address = stream
            .local_addr()
            .map_err(LocalHttpError::SocketSetupFailed)?;

        let sender = match connector_and_name {
            None => HttpSender::handshake(version, stream).await?,
            Some((connector, name)) => {
                let stream = connector
                    .connect(name, stream)
                    .await
                    .map_err(LocalHttpError::ConnectTlsFailed)?;
                HttpSender::handshake(version, Box::new(stream)).await?
            }
        };

        Ok(LocalHttpClient {
            sender,
            local_server_address,
            address,
            uses_tls,
        })
    }
}

/// Cleans up stale [`LocalHttpClient`]s from the [`ClientStore`].
async fn cleanup_task(store: ClientStore, idle_client_timeout: Duration) {
    let clients = Arc::downgrade(&store.clients);
    let notify = store.notify.clone();
    std::mem::drop(store);

    loop {
        let Some(clients) = clients.upgrade() else {
            // Failed `upgrade` means that all `ClientStore` instances were dropped.
            // This task is no longer needed.
            break;
        };

        let now = Instant::now();
        let mut min_last_used = None;
        let notified = {
            let Ok(mut guard) = clients.lock() else {
                tracing::error!("ClientStore mutex is poisoned, this is a bug");
                return;
            };

            guard.retain(|client| {
                if client.last_used + idle_client_timeout > now {
                    // We determine how long to sleep before cleaning the store again.
                    min_last_used = min_last_used
                        .map(|previous| cmp::min(previous, client.last_used))
                        .or(Some(client.last_used));

                    true
                } else {
                    // We drop the idle clients that have gone beyond the timeout.
                    tracing::trace!(?client, "Dropping an idle client");
                    false
                }
            });

            // Acquire [`Notified`] while still holding the lock.
            // Prevents missed updates.
            notify.notified()
        };

        if let Some(min_last_used) = min_last_used {
            time::sleep_until(min_last_used + idle_client_timeout).await;
        } else {
            notified.await;
        }
    }
}

#[cfg(test)]
mod test {
    use std::{convert::Infallible, time::Duration};

    use bytes::Bytes;
    use http_body_util::Empty;
    use hyper::{
        body::Incoming, server::conn::http1, service::service_fn, Request, Response, Version,
    };
    use hyper_util::rt::TokioIo;
    use mirrord_protocol::tcp::HttpRequestTransportType;
    use tokio::{net::TcpListener, time};

    use super::ClientStore;

    /// Verifies that [`ClientStore`] cleans up unused connections.
    #[tokio::test]
    async fn cleans_up_unused_connections() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let service = service_fn(|_req: Request<Incoming>| {
                std::future::ready(Ok::<_, Infallible>(Response::new(Empty::<Bytes>::new())))
            });

            let (connection, _) = listener.accept().await.unwrap();
            std::mem::drop(listener);
            http1::Builder::new()
                .serve_connection(TokioIo::new(connection), service)
                .await
                .unwrap()
        });

        let client_store =
            ClientStore::new_with_timeout(Duration::from_millis(10), Default::default());
        let client = client_store
            .get(
                addr,
                Version::HTTP_11,
                &HttpRequestTransportType::Tcp,
                &"http://some.server.com".parse().unwrap(),
            )
            .await
            .unwrap();
        client_store.push_idle(client);

        time::sleep(Duration::from_millis(100)).await;

        assert!(client_store.clients.lock().unwrap().is_empty());
    }
}
