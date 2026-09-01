use std::{
    cmp,
    collections::HashSet,
    fmt,
    net::SocketAddr,
    ops::Not,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures::FutureExt;
use hyper::{Uri, Version};
use mirrord_protocol::tcp::IncomingTrafficTransportType;
use mirrord_tls_util::{MaybeTls, UriExt};
use rustls::pki_types::ServerName;
use tokio::{
    net::TcpStream,
    sync::Notify,
    time::{self, Instant},
};
use tokio_rustls::{TlsConnector, TlsStream};
use tracing::Level;

use super::{HttpSender, LocalHttpClient, LocalHttpError};
use crate::proxies::incoming::tls::LocalTlsSetup;

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
/// We ignore the fact that [`IncomingTrafficTransportType::Tls::alpn_protocol`] and
/// [`IncomingTrafficTransportType::Tls::server_name`] might be different.
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
    /// Addresses of local servers that rejected a cleartext HTTP/2 connection preface.
    ///
    /// Requests for these servers are sent over HTTP/1, so that only the first one pays for a
    /// handshake that is known to fail. An entry is never removed, so a local application that
    /// gains HTTP/2 support is only talked to over HTTP/2 in the next session.
    http1_only_servers: Arc<Mutex<HashSet<SocketAddr>>>,
}

impl ClientStore {
    /// Creates a new store.
    ///
    /// The store will keep unused clients alive for at least the given time.
    pub fn new_with_timeout(timeout: Duration, tls_setup: Option<Arc<LocalTlsSetup>>) -> Self {
        let store = Self {
            clients: Default::default(),
            notify: Default::default(),
            tls_setup,
            http1_only_servers: Default::default(),
        };

        // Only spawn cleanup task if connection pooling is enabled
        if Self::should_enable_connection_pooling() {
            tokio::spawn(cleanup_task(store.clone(), timeout));
        }

        store
    }

    /// Determines whether connection pooling should be enabled.
    ///
    /// On Windows, connection pooling was previously disabled due to "channel closed" errors
    /// that occur when reusing HTTP connections in rapid succession scenarios.
    /// However, this was causing issues with HTTP mirroring, so we're re-enabling it.
    #[inline]
    fn should_enable_connection_pooling() -> bool {
        // Re-enabled for Windows to fix HTTP mirroring issues
        true
    }

    /// Reuses or creates a new [`LocalHttpClient`].
    #[tracing::instrument(
        level = Level::DEBUG,
        skip(self),
        ret, err(level = Level::DEBUG),
    )]
    pub async fn get(
        &self,
        server_addr: SocketAddr,
        version: Version,
        transport: &IncomingTrafficTransportType,
        request_uri: &Uri,
    ) -> Result<LocalHttpClient, LocalHttpError> {
        let version = self.resolve_version(server_addr, version, transport);

        if Self::should_enable_connection_pooling() {
            self.get_with_pooling(server_addr, version, transport, request_uri)
                .await
        } else {
            self.get_without_pooling(server_addr, version, transport, request_uri)
                .await
        }
    }

    /// Whether a connection made with the given transport is wrapped in TLS.
    fn uses_tls(&self, transport: &IncomingTrafficTransportType) -> bool {
        matches!(transport, IncomingTrafficTransportType::Tls { .. }) && self.tls_setup.is_some()
    }

    /// Returns the HTTP [`Version`] to use when talking with the given local server.
    ///
    /// This is the request's own version, except for a request that is to be sent in cleartext to
    /// a server known not to speak HTTP/2. See [`Self::http1_only_servers`].
    fn resolve_version(
        &self,
        server_addr: SocketAddr,
        version: Version,
        transport: &IncomingTrafficTransportType,
    ) -> Version {
        if version != Version::HTTP_2 || self.uses_tls(transport) {
            return version;
        }

        let known_http1_only = self
            .http1_only_servers
            .lock()
            .expect("ClientStore mutex is poisoned, this is a bug")
            .contains(&server_addr);

        if known_http1_only {
            tracing::debug!(
                %server_addr,
                "Local server does not speak cleartext HTTP/2, sending the request over HTTP/1",
            );

            Version::HTTP_11
        } else {
            version
        }
    }

    /// Gets a client with connection pooling (reuses existing connections).
    async fn get_with_pooling(
        &self,
        server_addr: SocketAddr,
        version: Version,
        transport: &IncomingTrafficTransportType,
        request_uri: &Uri,
    ) -> Result<LocalHttpClient, LocalHttpError> {
        let uses_tls = self.uses_tls(transport);

        if let Some(ready) = self
            .wait_for_ready(server_addr, version, uses_tls)
            .now_or_never()
        {
            tracing::debug!(?ready, "Reused an idle client");
            return Ok(ready);
        }

        tokio::select! {
            biased;

            ready = self.wait_for_ready(server_addr, version, uses_tls) => {
                tracing::debug!(?ready, "Reused an idle client");
                Ok(ready)
            },

            result = self.make_client(server_addr, version, transport, request_uri) => {
                let client = result?;
                tracing::debug!(?client, "Made a new client");
                Ok(client)
            },
        }
    }

    /// Gets a client without connection pooling (always creates new connections).
    async fn get_without_pooling(
        &self,
        server_addr: SocketAddr,
        version: Version,
        transport: &IncomingTrafficTransportType,
        request_uri: &Uri,
    ) -> Result<LocalHttpClient, LocalHttpError> {
        let client = self
            .make_client(server_addr, version, transport, request_uri)
            .await?;
        tracing::debug!(?client, "Created new HTTP client");
        Ok(client)
    }

    /// Records that a request failed on the given client.
    ///
    /// A cleartext HTTP/2 connection is made with prior knowledge - there is no negotiation, and a
    /// server that speaks only HTTP/1 rejects the connection preface. [`hyper`] sends the preface
    /// optimistically, so this shows up as the first request on the connection failing at the
    /// protocol level, and never as a failed handshake.
    ///
    /// When that happens, the local server is remembered as one to talk HTTP/1 to. The request is
    /// HTTP/2 because the remote server speaks it, which says nothing about the local application,
    /// and an application that cannot be connected to cannot be developed against.
    ///
    /// A connection that has already served a request, or that failed because it was closed, tells
    /// us nothing about the protocol - a local server is free to end a connection at any point.
    pub fn note_send_failure(&self, client: &LocalHttpClient, error: &LocalHttpError) {
        if client.handled_request() || client.uses_tls() || client.is_http_2().not() {
            return;
        }

        let LocalHttpError::SendFailed(error) = error else {
            return;
        };

        let connection_lost = error.is_closed()
            || error.is_canceled()
            || error.is_timeout()
            || error.is_incomplete_message();
        if connection_lost {
            return;
        }

        tracing::debug!(
            %error,
            server_addr = %client.local_server_address(),
            "Local server rejected an HTTP/2 request on a new connection, \
            treating it as an HTTP/1 server",
        );

        self.http1_only_servers
            .lock()
            .expect("ClientStore mutex is poisoned, this is a bug")
            .insert(client.local_server_address());
    }

    /// Stores an unused [`LocalHttpClient`], so that it can be reused later.
    #[tracing::instrument(level = Level::TRACE, skip(self))]
    pub fn push_idle(&self, client: LocalHttpClient) {
        if Self::should_enable_connection_pooling() {
            self.push_idle_with_pooling(client);
        } else {
            self.push_idle_without_pooling(client);
        }
    }

    /// Stores a client for reuse (connection pooling enabled).
    fn push_idle_with_pooling(&self, client: LocalHttpClient) {
        // A local application is free to end the connection after answering, and one that responds
        // with `Connection: close` always does. Caching such a client only guarantees that a later
        // request draws it from the store, fails to send on it, and pays a retry backoff before
        // getting the connection it could have had immediately.
        if client.is_closed() {
            tracing::trace!(
                ?client,
                "Dropping a client whose connection with the local application is already closed",
            );
            return;
        }

        let idle_client = IdleLocalClient {
            client,
            last_used: Instant::now(),
        };

        let Ok(mut guard) = self.clients.lock() else {
            tracing::error!("ClientStore mutex is poisoned, this is a bug");
            return;
        };

        guard.push(idle_client);
        self.notify.notify_one();
    }

    /// Drops a client immediately (connection pooling disabled).
    fn push_idle_without_pooling(&self, client: LocalHttpClient) {
        #[cfg(target_os = "windows")]
        tracing::trace!(
            ?client,
            "Dropping HTTP client (connection pooling disabled on Windows)"
        );

        #[cfg(not(target_os = "windows"))]
        tracing::trace!(
            ?client,
            "Dropping HTTP client (connection pooling disabled)"
        );

        std::mem::drop(client);
    }

    /// Waits until there is a ready unused client.
    #[tracing::instrument(level = Level::TRACE, skip_all, ret)]
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
                // A cached connection can be closed by the local application at any point while it
                // sits here, so being closed is checked when handing a client out and not only
                // when storing one.
                guard.retain(|idle| idle.client.is_closed().not());

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
    #[tracing::instrument(level = Level::TRACE, skip_all, ret, err(level = Level::TRACE))]
    async fn make_client(
        &self,
        local_server_address: SocketAddr,
        version: Version,
        transport: &IncomingTrafficTransportType,
        request_uri: &Uri,
    ) -> Result<LocalHttpClient, LocalHttpError> {
        let connector_and_name = match (transport, self.tls_setup.as_ref()) {
            (IncomingTrafficTransportType::Tcp, ..) => None,
            (.., None) => None,
            (
                IncomingTrafficTransportType::Tls {
                    alpn_protocol,
                    server_name: original_server_name,
                },
                Some(setup),
            ) => {
                let alpn_protocol = alpn_protocol.clone();
                let (connector, server_name) = setup.get(alpn_protocol).await?;

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

        let (stream, address) = connect(local_server_address, connector_and_name).await?;

        let sender = HttpSender::handshake(version, stream).await?;

        Ok(LocalHttpClient {
            sender,
            local_server_address,
            address,
            uses_tls,
            handled_request: false,
        })
    }
}

/// Makes a TCP connection with the given server, wrapping it in TLS if a connector is given.
///
/// Returns the connection and the address of its local socket.
async fn connect(
    local_server_address: SocketAddr,
    connector_and_name: Option<(TlsConnector, ServerName<'static>)>,
) -> Result<(MaybeTls, SocketAddr), LocalHttpError> {
    let stream = TcpStream::connect(local_server_address)
        .await
        .map_err(LocalHttpError::ConnectTcpFailed)?;
    // Stolen requests are relayed over this socket one at a time, so delaying small writes
    // with Nagle's algorithm only adds latency to every request. Failing to set it costs
    // latency, not correctness, so it must not fail the connection.
    if let Err(error) = stream.set_nodelay(true) {
        tracing::warn!(%error, %local_server_address, "Failed to set TCP_NODELAY on a local HTTP connection");
    }
    let address = stream
        .local_addr()
        .map_err(LocalHttpError::SocketSetupFailed)?;

    let stream = match connector_and_name {
        Some((connector, name)) => {
            let stream = connector
                .connect(name, stream)
                .await
                .map_err(LocalHttpError::ConnectTlsFailed)?;
            MaybeTls::Tls(Box::new(TlsStream::Client(stream)))
        }
        None => MaybeTls::NoTls(stream),
    };

    Ok((stream, address))
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
    use std::{convert::Infallible, net::SocketAddr, ops::Not, sync::Arc, time::Duration};

    use bytes::Bytes;
    use http_body_util::Empty;
    use hyper::{
        Method, Request, Response, Version,
        body::Incoming,
        server::conn::{http1, http2},
        service::service_fn,
    };
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use mirrord_protocol::tcp::{HttpRequest, IncomingTrafficTransportType, InternalHttpRequest};
    use rcgen::{
        BasicConstraints, CertificateParams, CertifiedKey, DnType, DnValue, IsCa, Issuer, KeyPair,
        KeyUsagePurpose,
    };
    use rustls::ServerConfig;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        time,
    };
    use tokio_rustls::TlsAcceptor;

    use super::{ClientStore, HttpSender};
    use crate::proxies::incoming::{http::StreamingBody, tls::LocalTlsSetup};

    /// Makes a request as [`ClientStore`]'s users make it out of a stolen HTTP/2 request: version
    /// [`Version::HTTP_2`], target in absolute form, and no `Host` header.
    fn http2_request(port: u16) -> HttpRequest<StreamingBody> {
        HttpRequest {
            connection_id: 0,
            request_id: 0,
            port,
            internal_request: InternalHttpRequest {
                method: Method::GET,
                uri: "http://some.server.com/api/v1".parse().unwrap(),
                headers: Default::default(),
                version: Version::HTTP_2,
                body: StreamingBody::default(),
            },
        }
    }

    /// Verifies that an idle HTTP/1 client is not reused for an HTTP/2 request.
    ///
    /// Reusing one silently converts the request to HTTP/1, which makes the protocol the local
    /// application sees depend on what happens to be in the store.
    #[tokio::test]
    async fn does_not_reuse_http1_client_for_http2_request() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let service = service_fn(|_req: Request<Incoming>| {
                std::future::ready(Ok::<_, Infallible>(Response::new(Empty::<Bytes>::new())))
            });

            let (http1_connection, _) = listener.accept().await.unwrap();
            tokio::spawn(
                http1::Builder::new().serve_connection(TokioIo::new(http1_connection), service),
            );

            let (http2_connection, _) = listener.accept().await.unwrap();
            tokio::spawn(
                http2::Builder::new(TokioExecutor::default())
                    .serve_connection(TokioIo::new(http2_connection), service),
            );
        });

        let client_store =
            ClientStore::new_with_timeout(Duration::from_secs(60), Default::default());
        let http1_client = client_store
            .get(
                addr,
                Version::HTTP_11,
                &IncomingTrafficTransportType::Tcp,
                &"http://some.server.com".parse().unwrap(),
            )
            .await
            .unwrap();
        client_store.push_idle(http1_client);

        let client = client_store
            .get(
                addr,
                Version::HTTP_2,
                &IncomingTrafficTransportType::Tcp,
                &"http://some.server.com".parse().unwrap(),
            )
            .await
            .unwrap();

        assert!(
            matches!(client.sender, HttpSender::V2(..)),
            "an HTTP/2 request must not be sent over an HTTP/1 connection: {client:?}",
        );
        assert_eq!(
            client_store.clients.lock().unwrap().len(),
            1,
            "the idle HTTP/1 client should have been left in the store",
        );
    }

    /// Verifies that a local server which rejects the cleartext HTTP/2 connection preface is
    /// talked to over HTTP/1 from then on.
    ///
    /// A cleartext HTTP/2 connection is made with prior knowledge, so a local application that
    /// speaks only HTTP/1 would otherwise be unreachable whenever the remote server speaks
    /// HTTP/2.
    #[tokio::test]
    async fn remembers_servers_that_reject_http2() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            // Answers the connection preface the way a server that speaks only HTTP/1 does.
            let (mut rejected, _) = listener.accept().await.unwrap();
            let mut preface = Vec::new();
            rejected.read_buf(&mut preface).await.unwrap();
            rejected
                .write_all(b"HTTP/1.1 400 Bad Request\r\ncontent-length: 0\r\n\r\n")
                .await
                .unwrap();
            std::mem::drop(rejected);

            let service = service_fn(|_req: Request<Incoming>| {
                std::future::ready(Ok::<_, Infallible>(Response::new(Empty::<Bytes>::new())))
            });
            let (connection, _) = listener.accept().await.unwrap();
            tokio::spawn(http1::Builder::new().serve_connection(TokioIo::new(connection), service));

            // Holds the listener, so that a third connection attempt is not refused but seen.
            std::future::pending::<()>().await;
        });

        let client_store =
            ClientStore::new_with_timeout(Duration::from_secs(60), Default::default());
        let mut client = client_store
            .get(
                addr,
                Version::HTTP_2,
                &IncomingTrafficTransportType::Tcp,
                &"http://some.server.com".parse().unwrap(),
            )
            .await
            .unwrap();
        assert!(
            client.is_http_2(),
            "the store should have made an HTTP/2 client for an HTTP/2 request: {client:?}",
        );

        // `hyper` sends the connection preface optimistically, so the rejection surfaces here and
        // not in the handshake.
        let error = client
            .send_request(http2_request(addr.port()))
            .await
            .expect_err("the local server should have rejected the HTTP/2 request");
        client_store.note_send_failure(&client, &error);
        std::mem::drop(client);

        let mut client = client_store
            .get(
                addr,
                Version::HTTP_2,
                &IncomingTrafficTransportType::Tcp,
                &"http://some.server.com".parse().unwrap(),
            )
            .await
            .unwrap();
        assert!(
            client.is_http_2().not(),
            "the store should have made an HTTP/1 client for a server that rejected HTTP/2: \
            {client:?}",
        );

        let response = client
            .send_request(http2_request(addr.port()))
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
    }

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
                &IncomingTrafficTransportType::Tcp,
                &"http://some.server.com".parse().unwrap(),
            )
            .await
            .unwrap();
        client_store.push_idle(client);

        time::sleep(Duration::from_millis(100)).await;

        assert!(client_store.clients.lock().unwrap().is_empty());
    }

    /// Verifies that [`ClientStore`] does not cache a client whose connection the local
    /// application has closed.
    ///
    /// Servers end connections routinely - answering with `Connection: close` is enough - and a
    /// cached dead client makes some later request fail its first send attempt and wait out a
    /// retry backoff for nothing.
    #[tokio::test]
    async fn does_not_cache_closed_connections() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let service = service_fn(|_req: Request<Incoming>| {
                std::future::ready(Ok::<_, Infallible>(Response::new(Empty::<Bytes>::new())))
            });

            let (connection, _) = listener.accept().await.unwrap();
            std::mem::drop(listener);
            // Answers one request and then ends the connection, as a server responding with
            // `Connection: close` does.
            let _ = http1::Builder::new()
                .serve_connection(TokioIo::new(connection), service)
                .await;
        });

        let client_store =
            ClientStore::new_with_timeout(Duration::from_secs(60), Default::default());
        let mut client = client_store
            .get(
                addr,
                Version::HTTP_11,
                &IncomingTrafficTransportType::Tcp,
                &"http://some.server.com".parse().unwrap(),
            )
            .await
            .unwrap();

        let request = HttpRequest {
            connection_id: 0,
            request_id: 0,
            port: addr.port(),
            internal_request: InternalHttpRequest {
                method: Method::GET,
                uri: "/".parse().unwrap(),
                headers: [(hyper::header::CONNECTION, "close".parse().unwrap())]
                    .into_iter()
                    .collect(),
                version: Version::HTTP_11,
                body: StreamingBody::from(Vec::new()),
            },
        };
        client.send_request(request).await.unwrap();

        // The connection task needs a moment to observe the close and finish.
        time::sleep(Duration::from_millis(100)).await;
        assert!(
            client.is_closed(),
            "the test server should have closed the connection"
        );

        client_store.push_idle(client);

        assert!(
            client_store.clients.lock().unwrap().is_empty(),
            "a client with a closed connection must not be cached for reuse",
        );
    }

    /// Generates a new [`CertifiedKey`] with a random [`KeyPair`].
    fn generate_cert(
        name: &str,
        issuer: Option<&CertifiedKey<KeyPair>>,
        can_sign_others: bool,
    ) -> CertifiedKey<KeyPair> {
        let signing_key = KeyPair::generate().unwrap();

        let mut params = CertificateParams::new(vec![name.to_owned()]).unwrap();
        params
            .distinguished_name
            .push(DnType::CommonName, DnValue::Utf8String(name.into()));

        if can_sign_others {
            params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        }

        let cert = match issuer {
            Some(issuer) => {
                let issuer =
                    Issuer::from_ca_cert_der(issuer.cert.der(), &issuer.signing_key).unwrap();

                params.signed_by(&signing_key, &issuer).unwrap()
            }
            None => params.self_signed(&signing_key).unwrap(),
        };

        CertifiedKey { cert, signing_key }
    }

    /// Verifies that [`LocalHttpClient`](super::LocalHttpClient) created with the [`ClientStore`]
    /// does not perform HTTP/1 upgrade to HTTP/2 when the connection is wrapped in TLS and ALPN
    /// already handles the upgrade.
    #[tokio::test]
    async fn no_http1_upgrade_after_alpn_upgrade() {
        let _ = rustls::crypto::CryptoProvider::install_default(
            rustls::crypto::aws_lc_rs::default_provider(),
        );

        let acceptor = {
            let issuer = generate_cert("issuer", None, true);
            let server = generate_cert("server", Some(&issuer), false);

            let mut config = ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![server.cert.into(), issuer.cert.into()],
                    server.signing_key.serialize_der().try_into().unwrap(),
                )
                .unwrap();
            config.alpn_protocols = vec![b"h2".into()];
            TlsAcceptor::from(Arc::new(config))
        };

        let request = HttpRequest {
            request_id: 0,
            connection_id: 0,
            port: 443,
            internal_request: InternalHttpRequest {
                method: Method::GET,
                uri: "https://well.com".parse().unwrap(),
                headers: Default::default(),
                version: Version::HTTP_2,
                body: StreamingBody::default(),
            },
        };

        let listener = TcpListener::bind("127.0.0.1:0".parse::<SocketAddr>().unwrap())
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let client_store = ClientStore::new_with_timeout(
                Duration::ZERO,
                LocalTlsSetup::from_config(Default::default()),
            );

            let mut client = client_store
                .make_client(
                    addr,
                    request.internal_request.version,
                    &IncomingTrafficTransportType::Tls {
                        alpn_protocol: Some(b"h2".into()),
                        server_name: None,
                    },
                    &request.internal_request.uri,
                )
                .await
                .unwrap();

            let _ = client.send_request(request).await;
        });

        let (conn, _) = listener.accept().await.unwrap();
        let mut conn = acceptor.accept(conn).await.unwrap();
        assert_eq!(conn.get_ref().1.alpn_protocol(), Some(b"h2".as_slice()));

        let mut first_bytes = [0_u8; 14];
        conn.read_exact(&mut first_bytes).await.unwrap();
        assert_eq!(first_bytes.as_slice(), b"PRI * HTTP/2.0".as_slice());
    }
}
