use std::{
    collections::HashMap,
    fmt, fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use mirrord_agent_env::steal_tls::{
    AgentClientAuth, AgentServerAuth, AlpnProtocol, RemoteClientAuth, RemoteServerAuth,
    StealTlsConfig,
};
use mirrord_tls_util::{
    rustls::{
        self,
        pki_types::ServerName,
        server::{VerifierBuilderError, WebPkiClientVerifier},
        ClientConfig, RootCertStore, ServerConfig,
    },
    tokio_rustls::{client, server, TlsAcceptor, TlsConnector},
    CertChain, Certs, DangerousNoVerifier, NicePath, RandomCert, TlsUtilError,
};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    task::JoinError,
};
use tracing::Level;

use crate::file::RootPath;

#[derive(Error, Debug)]
pub enum SetupError {
    #[error("failed to read certificate chain: {0}")]
    ReadCertChainError(#[source] TlsUtilError),
    #[error("failed to generate a dummy certificate: {0}")]
    GenerateDummyCertError(#[source] TlsUtilError),
    #[error("generated an invalid dummy certificate: {0}")]
    DummyCertInvalid(#[source] rustls::Error),
    #[error("no good root certificate was found")]
    NoGoodRoot,
    #[error("failed to build the peer verifier: {0}")]
    BuildVerifierError(#[from] VerifierBuilderError),
    #[error("certificate chain is invalid: {0}")]
    CertChainInvalid(#[source] rustls::Error),
    #[error("failed to resolve path `{path}` in the target container filesystem: {error}")]
    PathResolutionError { path: PathBuf, error: io::Error },
}

#[derive(Error, Debug)]
pub enum StealTlsError {
    #[error("background task panicked")]
    BackgroundTaskPanicked,
    #[error("failed to prepare mirrord-agent's TLS server: {0}")]
    ServerSetupError(#[source] SetupError),
    #[error("failed to prepare mirrord-agent's TLS client: {0}")]
    ClientSetupError(#[source] SetupError),
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
    #[allow(unused)]
    connector: TlsConnector,
}

impl StealTlsHandler {
    pub(crate) async fn accept<IO>(&self, stream: IO) -> io::Result<server::TlsStream<IO>>
    where
        IO: AsyncRead + AsyncWrite + Unpin,
    {
        self.acceptor.accept(stream).await
    }

    #[allow(unused)]
    pub(crate) async fn connect<IO>(
        &self,
        server_name: ServerName<'static>,
        stream: IO,
    ) -> io::Result<client::TlsStream<IO>>
    where
        IO: AsyncRead + AsyncWrite + Unpin,
    {
        self.connector.connect(server_name, stream).await
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
    const ACCEPTOR_VALIDITY: Duration = Duration::from_secs(60);

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
        let handler = tokio::task::spawn_blocking(move || {
            let acceptor = this
                .build_acceptor(&config.agent_server_auth)
                .map_err(StealTlsError::ServerSetupError)?;
            let connector = this
                .build_connector(&config.agent_client_auth)
                .map_err(StealTlsError::ClientSetupError)?;

            Ok::<_, StealTlsError>(StealTlsHandler {
                acceptor,
                connector,
                built_at: Instant::now(),
            })
        })
        .await??;

        if let Ok(mut resolved) = self.0.resolved.lock() {
            resolved.insert(port, handler.clone());
        } else {
            tracing::error!("Steal TLS handlers mutex is poisoned");
        }

        Ok(Some(handler))
    }

    #[tracing::instrument(level = Level::TRACE, err(level = Level::TRACE))]
    fn resolve_in_target_filesystem<'a>(
        &self,
        path: &'a Path,
    ) -> Result<ResolvedPath<'a>, SetupError> {
        let resolved = self.0.root_path.resolve_path(path).map_err(|error| {
            SetupError::PathResolutionError {
                path: path.to_path_buf(),
                error,
            }
        })?;

        Ok(ResolvedPath {
            requested: path,
            resolved,
        })
    }

    #[tracing::instrument(level = Level::DEBUG, err(level = Level::DEBUG))]
    fn build_acceptor(&self, config: &AgentServerAuth) -> Result<TlsAcceptor, SetupError> {
        let cert_pem = self.resolve_in_target_filesystem(&config.cert_pem)?;
        let cert_key = self.resolve_in_target_filesystem(&config.key_pem)?;
        let (cert_chain, key_der) = CertChain::read(&cert_pem, &cert_key)
            .map_err(SetupError::ReadCertChainError)?
            .into_chain_and_key();

        let builder = match &config.client_auth {
            Some(RemoteClientAuth {
                allow_anonymous,
                root_cert_pems,
            }) => {
                let mut store = self.build_root_store(root_cert_pems);

                if store.is_empty() {
                    if *allow_anonymous {
                        let dummy = RandomCert::generate(vec!["dummy".into()])
                            .map_err(SetupError::GenerateDummyCertError)?;
                        store
                            .add(dummy.into())
                            .map_err(SetupError::DummyCertInvalid)?;
                    } else {
                        return Err(SetupError::NoGoodRoot);
                    }
                }

                let mut builder = WebPkiClientVerifier::builder(store.into());
                if *allow_anonymous {
                    builder = builder.allow_unauthenticated();
                }

                let verifier = builder.build().map_err(SetupError::from)?;

                ServerConfig::builder().with_client_cert_verifier(verifier)
            }
            None => ServerConfig::builder().with_no_client_auth(),
        };

        let mut server_config = builder
            .with_single_cert(cert_chain, key_der)
            .map_err(SetupError::CertChainInvalid)?;
        server_config.alpn_protocols = config
            .alpn_protocols
            .iter()
            .filter_map(AlpnProtocol::as_bytes)
            .map(Vec::from)
            .collect();

        Ok(TlsAcceptor::from(Arc::new(server_config)))
    }

    #[tracing::instrument(level = Level::DEBUG, ret)]
    fn build_root_store(&self, paths: &[PathBuf]) -> RootCertStore {
        let mut root_store = RootCertStore::empty();

        let mut queue = Vec::with_capacity(paths.len());
        for path in paths {
            match self.0.root_path.resolve_path(path) {
                Ok(resolved) => queue.push((resolved, true)),
                Err(error) => {
                    tracing::error!(
                        %error,
                        ?path,
                        "Tracing failed to resolve a path in the target container file system \
                        when building a root cert store."
                    );
                }
            }
        }

        while let Some((path, read_if_dir)) = queue.pop() {
            let entries = if read_if_dir {
                match fs::read_dir(&path) {
                    Ok(entries) => Some(entries),
                    Err(error) if error.kind() == io::ErrorKind::NotADirectory => None,
                    Err(error) => {
                        tracing::error!(
                            %error,
                            ?path,
                            "Failed to access a file when building a root cert store."
                        );
                        continue;
                    }
                }
            } else {
                None
            };

            if let Some(entries) = entries {
                for entry in entries {
                    match entry {
                        Ok(entry) => {
                            queue.push((entry.path(), false));
                        }
                        Err(error) => {
                            tracing::error!(
                                %error,
                                ?path,
                                "Failed to list a directory entry when building a root cert store."
                            )
                        }
                    }
                }
                continue;
            }

            let certs = Certs::read(&path);
            for cert in certs {
                let cert = match cert {
                    Ok(cert) => {
                        tracing::trace!(?path, "Found a certificate");
                        cert
                    }
                    Err(error) => {
                        tracing::warn!(
                            ?path,
                            %error,
                            "Failed to parse a PEM file when building a root cert store.",
                        );
                        continue;
                    }
                };

                if let Err(error) = root_store.add(cert) {
                    tracing::warn!(
                        %error,
                        ?path,
                        "Found an invalid certificate when building a root cert store.",
                    )
                } else {
                    tracing::trace!(?path, "Successfully added a certificate to the root store.")
                }
            }
        }

        root_store
    }

    #[tracing::instrument(level = Level::DEBUG, err(level = Level::DEBUG))]
    fn build_connector(&self, config: &AgentClientAuth) -> Result<TlsConnector, SetupError> {
        let cert_pem = self.resolve_in_target_filesystem(&config.cert_pem)?;
        let key_pem = self.resolve_in_target_filesystem(&config.key_pem)?;
        let (cert_chain, key_der) = CertChain::read(&cert_pem, &key_pem)
            .map_err(SetupError::ReadCertChainError)?
            .into_chain_and_key();

        let builder = match &config.server_auth {
            Some(RemoteServerAuth { root_cert_pems }) => {
                let root_store = self.build_root_store(root_cert_pems);
                if root_store.is_empty() {
                    return Err(SetupError::NoGoodRoot);
                }
                ClientConfig::builder().with_root_certificates(root_store)
            }
            None => ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(DangerousNoVerifier)),
        };

        let config = builder
            .with_client_auth_cert(cert_chain, key_der)
            .map_err(SetupError::CertChainInvalid)?;

        Ok(TlsConnector::from(Arc::new(config)))
    }
}

impl fmt::Debug for StealTlsHandlers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StealTlsAcceptors")
            .field("config", &self.0.config)
            .field("root_path", &self.0.root_path)
            .field(
                "resolved",
                &self.0.resolved.lock().as_ref().map(|guard| &**guard),
            )
            .finish()
    }
}

struct ResolvedPath<'a> {
    requested: &'a Path,
    resolved: PathBuf,
}

impl NicePath for ResolvedPath<'_> {
    fn display_path(&self) -> &Path {
        self.requested
    }

    fn real_path(&self) -> &Path {
        &self.resolved
    }
}
