use std::{
    collections::HashMap,
    fmt,
    fs::File,
    io::{self, BufReader},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use http::Uri;
use mirrord_agent_env::steal_tls::{AlpnProtocol, StealPortTlsConfig, StealTlsConfig};
use rustls::{
    client::VerifierBuilderError,
    pki_types::{CertificateDer, InvalidDnsNameError, PrivateKeyDer, ServerName},
    server::{danger::ClientCertVerifier, NoClientAuth, WebPkiClientVerifier},
    ClientConfig, RootCertStore, ServerConfig,
};
use rustls_pemfile::Item;
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    task::JoinError,
};
use tokio_rustls::{client, server, TlsAcceptor, TlsConnector};
use tracing::Level;

use crate::file::RootPath;

#[derive(Error, Debug)]
pub enum FromPemError {
    #[error("failed to read the PEM file: {0}")]
    ReadError(#[source] io::Error),
    #[error("failed to parse the PEM file: {0}")]
    ParseError(#[source] io::Error),
    #[error("no certificate was found in the PEM file")]
    CertChainEmpty,
    #[error("no private key was found in the PEM file")]
    KeyNotFound,
    #[error("multiple private keys were found in the PEM file")]
    MultipleKeysFound,
}

#[derive(Error, Debug)]
pub enum StealTlsError {
    #[error("failed to process `{path}`: {error}")]
    FromPemError {
        #[source]
        error: FromPemError,
        path: PathBuf,
    },
    #[error("failed to build mirrord-agent's TLS server config: {0}")]
    BuildServerConfigError(#[source] rustls::Error),
    #[error("failed to build client cert verifier for the mirrord-agent's TLS server: {0}")]
    BuildClientVerifierError(#[from] VerifierBuilderError),
    #[error("failed to build mirrord-agent's TLS client config: {0}")]
    BuildClientConfigError(#[source] rustls::Error),
    #[error("background task responsible for builing TLS configuration panicked")]
    BackgroundTaskPanicked,
}

#[derive(Error, Debug)]
pub enum ConnectTlsError {
    #[error("IO failed: {0}")]
    IoError(#[from] io::Error),
    #[error("request URI did not contain a valid DNS name: {0}")]
    InvalidDnsNameError(#[from] InvalidDnsNameError),
}

impl From<JoinError> for StealTlsError {
    fn from(_: JoinError) -> Self {
        Self::BackgroundTaskPanicked
    }
}

#[derive(Clone)]
pub(crate) struct StealTlsHandler {
    built_at: Instant,
    acceptor: TlsAcceptor,
    connector: TlsConnector,
}

impl StealTlsHandler {
    pub(crate) async fn accept<IO>(&self, stream: IO) -> io::Result<server::TlsStream<IO>>
    where
        IO: AsyncRead + AsyncWrite + Unpin,
    {
        self.acceptor.accept(stream).await
    }

    pub(crate) async fn connect<IO>(
        &self,
        uri: &Uri,
        stream: IO,
    ) -> Result<client::TlsStream<IO>, ConnectTlsError>
    where
        IO: AsyncRead + AsyncWrite + Unpin,
    {
        let mut hostname = uri.host().unwrap_or_default();

        // Remove square brackets around IPv6 address.
        if let Some(trimmed) = hostname.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
            hostname = trimmed;
        }

        let domain = ServerName::try_from(hostname)?.to_owned();

        self.connector
            .connect(domain, stream)
            .await
            .map_err(From::from)
    }
}

impl fmt::Debug for StealTlsHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StealTlsHandler")
            .field("age_ms", &self.built_at.elapsed().as_millis())
            .finish()
    }
}

#[derive(Default)]
struct State {
    config: StealTlsConfig,
    resolved: Mutex<HashMap<u16, StealTlsHandler>>,
    root_path: RootPath,
}

#[derive(Clone, Default)]
pub(crate) struct StealTlsHandlers(Arc<State>);

impl StealTlsHandlers {
    const ACCEPTOR_VALIDITY: Duration = Duration::from_secs(30);

    #[tracing::instrument(level = Level::DEBUG, ret)]
    pub(crate) fn new(config: StealTlsConfig, target_pid: u64) -> Self {
        Self(Arc::new(State {
            config,
            resolved: Default::default(),
            root_path: RootPath::new(target_pid),
        }))
    }

    #[tracing::instrument(level = Level::DEBUG, err(level = Level::ERROR))]
    pub(crate) async fn get_handler(
        &self,
        port: u16,
    ) -> Result<Option<StealTlsHandler>, StealTlsError> {
        let ready = self
            .0
            .resolved
            .lock()
            .inspect_err(|_| tracing::error!("Steal TLS handlers mutex is poisoned"))
            .ok()
            .and_then(|resolved| resolved.get(&port).cloned())
            .filter(|entry| entry.built_at.elapsed() < Self::ACCEPTOR_VALIDITY);
        if let Some(ready) = ready {
            return Ok(Some(ready));
        }

        let Some(config) = self.0.config.get(&port).cloned() else {
            return Ok(None);
        };

        let this = self.clone();
        let handler = tokio::task::spawn_blocking(move || this.build_handler(&config)).await??;

        if let Ok(mut resolved) = self.0.resolved.lock() {
            resolved.insert(port, handler.clone());
        } else {
            tracing::error!("Steal TLS handlers mutex is poisoned");
        }

        Ok(Some(handler))
    }

    #[tracing::instrument(level = Level::DEBUG, err(level = Level::DEBUG))]
    fn build_handler(&self, config: &StealPortTlsConfig) -> Result<StealTlsHandler, StealTlsError> {
        let acceptor = self.build_acceptor(&config)?;
        let connector = self.build_connector(&config)?;

        Ok(StealTlsHandler {
            acceptor,
            connector,
            built_at: Instant::now(),
        })
    }

    #[tracing::instrument(level = Level::DEBUG, err(level = Level::DEBUG))]
    fn build_acceptor(&self, config: &StealPortTlsConfig) -> Result<TlsAcceptor, StealTlsError> {
        let cert_chain = self
            .get_cert_chain(&config.remote_server_auth.cert_pem)
            .map_err(|error| StealTlsError::FromPemError {
                error,
                path: config.remote_server_auth.cert_pem.to_path_buf(),
            })?;
        let key = self
            .get_key(&config.remote_server_auth.key_pem)
            .map_err(|error| StealTlsError::FromPemError {
                error,
                path: config.remote_server_auth.key_pem.to_path_buf(),
            })?;

        let client_cert_verifier: Arc<dyn ClientCertVerifier> = if let Some(client_auth) =
            &config.remote_client_auth
        {
            let mut root_store = RootCertStore::empty();

            for path in &client_auth.root_cert_pems {
                self.add_all_to_root_store(&mut root_store, path);
            }

            if client_auth.allow_unauthenticated && root_store.is_empty() {
                tracing::debug!(
                        "Root store is empty, but rustls verifier needs at least one trust anchor, even if anonymous clients are allowed. \
                        Generating a dummy root certificate",
                    );
                Self::add_dummy_cert(&mut root_store);
            }

            let mut verifier = WebPkiClientVerifier::builder(Arc::new(root_store));
            if client_auth.allow_unauthenticated {
                verifier = verifier.allow_unauthenticated();
            }
            verifier.build()?
        } else {
            Arc::new(NoClientAuth)
        };

        let mut server_config = ServerConfig::builder()
            .with_client_cert_verifier(client_cert_verifier)
            .with_single_cert(cert_chain, key)
            .map_err(StealTlsError::BuildServerConfigError)?;

        server_config.alpn_protocols = config
            .remote_server_auth
            .alpn_protocols
            .iter()
            .filter_map(AlpnProtocol::as_bytes)
            .map(Vec::from)
            .collect();

        Ok(TlsAcceptor::from(Arc::new(server_config)))
    }

    #[tracing::instrument(level = Level::DEBUG, err(level = Level::DEBUG))]
    fn build_connector(&self, config: &StealPortTlsConfig) -> Result<TlsConnector, StealTlsError> {
        let mut root_store = RootCertStore::empty();
        self.add_all_to_root_store(&mut root_store, &config.remote_server_auth.cert_pem);

        let client_config = match &config.agent_client_auth {
            Some(agent_auth) => {
                let cert_chain = self.get_cert_chain(&agent_auth.cert_pem).map_err(|error| {
                    StealTlsError::FromPemError {
                        error,
                        path: agent_auth.cert_pem.clone(),
                    }
                })?;
                let key = self.get_key(&agent_auth.key_pem).map_err(|error| {
                    StealTlsError::FromPemError {
                        error,
                        path: agent_auth.key_pem.clone(),
                    }
                })?;

                ClientConfig::builder()
                    .with_root_certificates(root_store)
                    .with_client_auth_cert(cert_chain, key)
                    .map_err(StealTlsError::BuildClientConfigError)?
            }

            None => ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth(),
        };

        Ok(TlsConnector::from(Arc::new(client_config)))
    }

    #[tracing::instrument(level = Level::DEBUG)]
    fn add_all_to_root_store(&self, root_store: &mut RootCertStore, path: &Path) {
        let resolved = match self.0.root_path.resolve_path(path) {
            Ok(resolved) => resolved,
            Err(error) => {
                tracing::warn!(
                    ?path,
                    %error,
                    "Failed to resolve a root certificate path in the target container filesystem",
                );
                return;
            }
        };

        let mut file = match File::open(resolved) {
            Ok(file) => BufReader::new(file),
            Err(error) => {
                tracing::warn!(
                    ?path,
                    %error,
                    "Failed to open a root certificate file in the target container filesystem",
                );
                return;
            }
        };

        for cert in rustls_pemfile::certs(&mut file) {
            match cert {
                Ok(cert) => match root_store.add(cert) {
                    Ok(()) => {
                        tracing::debug!(?path, "Ceritificate added");
                    }
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            ?path,
                            "Failed to add a certificate to the root certificate store",
                        );
                    }
                },

                Err(error) => {
                    tracing::warn!(
                        ?path,
                        %error,
                        "Failed to parse a PEM file when building a root certificate store",
                    );

                    // Inspected `rustls_pemfile::certs` code
                    // and it looks like we can hit an endless loop if we don't break.
                    continue;
                }
            }
        }
    }

    /// Generates a dummy self-signed certificate and adds it to the given [`RootCertStore`].
    ///
    /// We do this, because [`WebPkiClientVerifier`] always requires a non-empty root cert store,
    /// even if anonymous clients are allowed.
    ///
    /// This function is best-effort. If something fails here, no certificate will be added.
    #[tracing::instrument(level = Level::DEBUG)]
    fn add_dummy_cert(root_store: &mut RootCertStore) {
        match rcgen::generate_simple_self_signed(["dummy".to_string()]) {
            Ok(cert) => {
                if let Err(error) = root_store.add(cert.cert.into()) {
                    tracing::debug!(
                        %error,
                        "Failed to add the certificate",
                    );
                }
            }
            Err(error) => {
                tracing::debug!(
                    %error,
                    "Failed to generate the certificate",
                );
            }
        }
    }

    #[tracing::instrument(level = Level::DEBUG, err(level = Level::DEBUG))]
    fn get_cert_chain(&self, path: &Path) -> Result<Vec<CertificateDer<'static>>, FromPemError> {
        let mut pem = self
            .0
            .root_path
            .resolve_path(path)
            .and_then(|path| File::open(path))
            .map(BufReader::new)
            .map_err(FromPemError::ReadError)?;

        let cert_chain = rustls_pemfile::certs(&mut pem)
            .collect::<Result<Vec<_>, _>>()
            .map_err(FromPemError::ParseError)?;

        if cert_chain.is_empty() {
            return Err(FromPemError::CertChainEmpty);
        }

        Ok(cert_chain)
    }

    #[tracing::instrument(level = Level::DEBUG, err(level = Level::DEBUG))]
    fn get_key(&self, path: &Path) -> Result<PrivateKeyDer<'static>, FromPemError> {
        let mut pem = self
            .0
            .root_path
            .resolve_path(path)
            .and_then(|path| File::open(path))
            .map(BufReader::new)
            .map_err(FromPemError::ReadError)?;

        let mut found_key = None;

        for entry in rustls_pemfile::read_all(&mut pem) {
            let entry = entry.map_err(FromPemError::ParseError)?;
            let key = match entry {
                Item::Pkcs1Key(key) => PrivateKeyDer::Pkcs1(key),
                Item::Pkcs8Key(key) => PrivateKeyDer::Pkcs8(key),
                Item::Sec1Key(key) => PrivateKeyDer::Sec1(key),
                _ => continue,
            };

            if found_key.replace(key).is_some() {
                return Err(FromPemError::MultipleKeysFound);
            }
        }

        found_key.ok_or(FromPemError::KeyNotFound)
    }
}

impl fmt::Debug for StealTlsHandlers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StealTlsAcceptors")
            .field("config", &self.0.config)
            .field("root_path", &self.0.root_path)
            .field(
                "resolved",
                &self.0.resolved.lock().as_ref().map(|guard| &*guard),
            )
            .finish()
    }
}
