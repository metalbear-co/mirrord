use std::{
    cmp, fmt,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use hyper::Version;
use tokio::{
    sync::Notify,
    time::{self, Instant},
};
use tracing::Level;

use super::{LocalHttpClient, LocalHttpError};

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
/// dedicated [`tokio::task`]. This timeout defaults to [`Self::IDLE_CLIENT_DEFAULT_TIMEOUT`].
#[derive(Clone)]
pub struct ClientStore {
    clients: Arc<Mutex<Vec<IdleLocalClient>>>,
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
    pub fn new_with_timeout(timeout: Duration) -> Self {
        let store = Self {
            clients: Default::default(),
            notify: Default::default(),
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
    ) -> Result<LocalHttpClient, LocalHttpError> {
        let ready = {
            let mut guard = self
                .clients
                .lock()
                .expect("ClientStore mutex is poisoned, this is a bug");
            let position = guard.iter().position(|idle| {
                idle.client.handles_version(version)
                    && idle.client.local_server_address() == server_addr
            });
            position.map(|position| guard.swap_remove(position))
        };

        if let Some(ready) = ready {
            tracing::trace!(?ready, "Reused an idle client");
            return Ok(ready.client);
        }

        let connect_task = tokio::spawn(LocalHttpClient::new(server_addr, version));

        tokio::select! {
            result = connect_task => result.expect("this task should not panic"),
            ready = self.wait_for_ready(server_addr, version) => {
                tracing::trace!(?ready, "Reused an idle client");
                Ok(ready)
            },
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
    async fn wait_for_ready(&self, server_addr: SocketAddr, version: Version) -> LocalHttpClient {
        loop {
            let notified = {
                let mut guard = self
                    .clients
                    .lock()
                    .expect("ClientStore mutex is poisoned, this is a bug");
                let position = guard.iter().position(|idle| {
                    idle.client.handles_version(version)
                        && idle.client.local_server_address() == server_addr
                });

                match position {
                    Some(position) => return guard.swap_remove(position).client,
                    None => self.notify.notified(),
                }
            };

            notified.await;
        }
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

        let client_store = ClientStore::new_with_timeout(Duration::from_millis(10));
        let client = client_store.get(addr, Version::HTTP_11).await.unwrap();
        client_store.push_idle(client);

        time::sleep(Duration::from_millis(100)).await;

        assert!(client_store.clients.lock().unwrap().is_empty());
    }
}
