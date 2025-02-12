use std::{
    collections::HashMap,
    fmt, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use mirrord_agent_env::steal_tls::{
    AgentClientAuth, AlpnProtocol, RemoteClientAuth, StealPortTlsConfig, StealTlsConfig,
};
use mirrord_tls_util::{
    rustls::pki_types::ServerName,
    tokio_rustls::{client, server, TlsAcceptor, TlsConnector},
    AcceptorClientAuth, BestEffortRootStore, CertWithKey, ConnectorClientAuth, ConnectorServerAuth,
    TlsAcceptorConfig, TlsAcceptorExt, TlsConnectorConfig, TlsConnectorExt, TlsUtilError,
};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    task::JoinError,
};
use tracing::Level;

use crate::file::RootPath;

#[derive(Error, Debug)]
pub enum StealTlsError {
    #[error("failed to resolve path `{path}` in the target container filesystem: {error}")]
    PathResolutionError { path: PathBuf, error: io::Error },
    #[error(transparent)]
    TlsUtilError(#[from] TlsUtilError),
    #[error("produced an empty root certificates store")]
    EmptyRootCertStore,
    #[error("background task responsible for builing TLS configuration panicked")]
    BackgroundTaskPanicked,
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
        self.connector
            .connect(server_name, stream)
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

    #[tracing::instrument(level = Level::TRACE, err(level = Level::TRACE))]
    fn resolve_in_target_filesystem(&self, path: &Path) -> Result<PathBuf, StealTlsError> {
        self.0
            .root_path
            .resolve_path(path)
            .map_err(|error| StealTlsError::PathResolutionError {
                path: path.to_path_buf(),
                error,
            })
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
        let cert_pem = self.resolve_in_target_filesystem(&config.remote_server_auth.cert_pem)?;
        let cert_key = self.resolve_in_target_filesystem(&config.remote_server_auth.key_pem)?;
        let server_auth = CertWithKey::read(&cert_pem, &cert_key).map_err(TlsUtilError::from)?;

        let client_auth = match &config.remote_client_auth {
            Some(RemoteClientAuth {
                allow_unauthenticated,
                root_cert_pems,
            }) => {
                let mut store = BestEffortRootStore::default();

                for path in root_cert_pems {
                    let Ok(path) = self.resolve_in_target_filesystem(path) else {
                        continue;
                    };

                    store.try_add_all_from_pem(&path);
                }

                if !*allow_unauthenticated && store.certs() == 0 {
                    return Err(StealTlsError::EmptyRootCertStore);
                }

                AcceptorClientAuth::WithRootStore {
                    store,
                    allow_unauthenticated: *allow_unauthenticated,
                }
            }
            None => AcceptorClientAuth::Disabled,
        };

        let server_config = TlsAcceptorConfig {
            server_auth,
            client_auth,
            alpn_protocols: config
                .remote_server_auth
                .alpn_protocols
                .iter()
                .filter_map(AlpnProtocol::as_bytes)
                .map(Vec::from)
                .collect(),
        };

        TlsAcceptor::build_from_config(server_config).map_err(From::from)
    }

    #[tracing::instrument(level = Level::DEBUG, err(level = Level::DEBUG))]
    fn build_connector(&self, config: &StealPortTlsConfig) -> Result<TlsConnector, StealTlsError> {
        let path = self.resolve_in_target_filesystem(&config.remote_server_auth.cert_pem)?;
        let mut server_auth = BestEffortRootStore::default();
        server_auth.try_add_all_from_pem(&path);
        if server_auth.certs() == 0 {
            return Err(StealTlsError::EmptyRootCertStore);
        }

        let client_auth = match &config.agent_client_auth {
            Some(AgentClientAuth { cert_pem, key_pem }) => {
                let cert_pem = self.resolve_in_target_filesystem(cert_pem)?;
                let key_pem = self.resolve_in_target_filesystem(key_pem)?;
                let cert = CertWithKey::read(&cert_pem, &key_pem).map_err(TlsUtilError::from)?;
                ConnectorClientAuth::CertWithKey(cert)
            }

            None => ConnectorClientAuth::Anonymous,
        };

        let client_config = TlsConnectorConfig {
            server_auth: ConnectorServerAuth::BestEffortRootStore(server_auth),
            client_auth,
        };

        TlsConnector::build_from_config(client_config).map_err(From::from)
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
