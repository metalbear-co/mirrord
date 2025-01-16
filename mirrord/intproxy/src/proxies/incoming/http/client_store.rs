use std::{cmp, net::SocketAddr, time::Duration};

use hyper::Version;
use tokio::{
    sync::watch,
    time::{self, Instant},
};

use super::{LocalHttpClient, LocalHttpError};

#[derive(Debug)]
struct IdleLocalClient {
    client: LocalHttpClient,
    last_used: Instant,
}

#[derive(Clone)]
pub struct ClientStore(watch::Sender<Vec<IdleLocalClient>>);

impl Default for ClientStore {
    fn default() -> Self {
        Self::new_with_timeout(Self::IDLE_CLIENT_TIMEOUT)
    }
}

impl ClientStore {
    const IDLE_CLIENT_TIMEOUT: Duration = Duration::from_secs(3);

    pub fn new_with_timeout(timeout: Duration) -> Self {
        let (tx, _) = watch::channel(Default::default());

        tokio::spawn(cleanup_task(tx.clone(), timeout));

        Self(tx)
    }

    pub async fn get(
        &self,
        server_addr: SocketAddr,
        version: Version,
    ) -> Result<LocalHttpClient, LocalHttpError> {
        let mut ready = None;

        self.0.send_if_modified(|clients| {
            println!("ready clients: {clients:?}");
            let position = clients.iter().position(|idle| {
                idle.client.handles_version(version)
                    && idle.client.local_server_address() == server_addr
            });

            let Some(position) = position else {
                return false;
            };

            let client = clients.swap_remove(position).client;
            ready.replace(client);
            true
        });

        if let Some(ready) = ready {
            println!("found ready client");
            return Ok(ready);
        }

        let connect_task = tokio::spawn(LocalHttpClient::new(server_addr, version));

        tokio::select! {
            result = connect_task => result.expect("this task should not panic"),
            ready = self.wait_for_ready(server_addr, version) => Ok(ready),
        }
    }

    pub fn push_idle(&self, client: LocalHttpClient) {
        println!("storing idle client {client:?}");
        self.0.send_modify(|clients| {
            clients.push(IdleLocalClient {
                client,
                last_used: Instant::now(),
            })
        });
    }

    async fn wait_for_ready(&self, server_addr: SocketAddr, version: Version) -> LocalHttpClient {
        let mut recevier = self.0.subscribe();

        loop {
            let mut ready = None;

            self.0.send_if_modified(|clients| {
                let position = clients.iter().position(|idle| {
                    idle.client.handles_version(version)
                        && idle.client.local_server_address() == server_addr
                });
                let Some(position) = position else {
                    return false;
                };

                let client = clients.swap_remove(position).client;
                ready.replace(client);

                true
            });

            if let Some(ready) = ready {
                break ready;
            }

            recevier
                .changed()
                .await
                .expect("sender alive in this struct");
        }
    }
}

async fn cleanup_task(clients: watch::Sender<Vec<IdleLocalClient>>, idle_client_timeout: Duration) {
    loop {
        let now = Instant::now();
        let mut min_last_used = None;

        clients.send_if_modified(|clients| {
            let mut removed = false;

            clients.retain(|client| {
                if client.last_used + idle_client_timeout > now {
                    min_last_used = min_last_used
                        .map(|previous| cmp::min(previous, client.last_used))
                        .or(Some(client.last_used));

                    true
                } else {
                    removed = true;
                    false
                }
            });

            removed
        });

        if let Some(min_last_used) = min_last_used {
            time::sleep_until(min_last_used + idle_client_timeout).await;
        } else {
            clients
                .subscribe()
                .changed()
                .await
                .expect("sender alive in this function");
        }
    }
}

#[cfg(test)]
mod test {
    use std::{convert::Infallible, time::Duration};

    use bytes::Bytes;
    use http_body_util::Empty;
    use hyper::{body::Incoming, server::conn::http1, service::service_fn, Request, Response, Version};
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

        assert!(client_store.0.borrow().is_empty());
    }
}
