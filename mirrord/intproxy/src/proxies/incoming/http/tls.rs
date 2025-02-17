use std::{
    fs::{self, File},
    io::BufReader,
    path::PathBuf,
    sync::Arc,
};

use mirrord_tls_util::DangerousNoVerifierServer;
use rustls::{pki_types::ServerName, ClientConfig, RootCertStore};
use thiserror::Error;
use tokio::{sync::OnceCell, task::JoinError};
use tokio_rustls::TlsConnector;

#[derive(Debug, Error)]
pub enum LocalTlsSetupError {
    #[error("no good trust root certificate was found")]
    NoGoodRoot,
    #[error("background task panicked")]
    BackgroundTaskPanicked,
}

impl From<JoinError> for LocalTlsSetupError {
    fn from(_: JoinError) -> Self {
        Self::BackgroundTaskPanicked
    }
}

pub struct LazyTlsConnector {
    trust_roots: Option<Vec<PathBuf>>,
    config: OnceCell<ClientConfig>,
    server_name: Option<ServerName<'static>>,
}

impl LazyTlsConnector {
    pub fn new(
        trust_roots: Option<Vec<PathBuf>>,
        server_name: Option<ServerName<'static>>,
    ) -> Self {
        Self {
            trust_roots,
            config: Default::default(),
            server_name,
        }
    }

    pub async fn get(
        &self,
        alpn_protocol: Option<Vec<u8>>,
    ) -> Result<TlsConnector, LocalTlsSetupError> {
        let mut config = self
            .config
            .get_or_try_init(|| self.prepare_config())
            .await?
            .clone();
        config.alpn_protocols.extend(alpn_protocol);

        Ok(TlsConnector::from(Arc::new(config)))
    }

    pub fn server_name(&self) -> Option<&ServerName<'static>> {
        self.server_name.as_ref()
    }

    async fn prepare_config(&self) -> Result<ClientConfig, LocalTlsSetupError> {
        let builder = match self.trust_roots.as_ref() {
            Some(trust_roots) => {
                let paths = trust_roots.clone();
                let store =
                    tokio::task::spawn_blocking(move || Self::build_root_store(paths)).await?;

                if store.is_empty() {
                    return Err(LocalTlsSetupError::NoGoodRoot);
                }

                ClientConfig::builder().with_root_certificates(store)
            }
            None => ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(DangerousNoVerifierServer)),
        };

        Ok(builder.with_no_client_auth())
    }

    /// Builds a [`RootCertStore`] from all certificates founds under the given paths.
    ///
    /// Accepts paths to:
    /// 1. PEM files
    /// 2. Directories containing PEM files
    ///
    /// Directories are not traversed recursively.
    ///
    /// Does not fail, see [`RootCertStore::add_parsable_certificates`] for rationale.
    /// Instead, it logs warnings and errors.
    fn build_root_store(paths: Vec<PathBuf>) -> RootCertStore {
        let mut root_store = RootCertStore::empty();

        let mut queue = paths.into_iter().map(|p| (p, true)).collect::<Vec<_>>();

        while let Some((path, read_if_dir)) = queue.pop() {
            let is_dir = path.is_dir();

            if is_dir && read_if_dir {
                let Ok(entries) = fs::read_dir(&path).inspect_err(|error| {
                    tracing::error!(
                        %error,
                        ?path,
                        "Failed to list a directory when building a root cert store."
                    );
                }) else {
                    continue;
                };

                entries
                    .filter_map(|result| {
                        result
                            .inspect_err(|error| {
                                tracing::error!(
                                    %error,
                                    ?path,
                                    "Failed to list a directory when building a root cert store."
                                )
                            })
                            .ok()
                    })
                    .for_each(|entry| queue.push((entry.path(), false)))
            } else if is_dir {
                continue;
            } else {
                let Ok(file) = File::open(&path).inspect_err(|error| {
                    tracing::error!(
                        %error,
                        ?path,
                        "Failed to open a file when building a root cert store."
                    );
                }) else {
                    continue;
                };

                let mut file = BufReader::new(file);
                let certs = rustls_pemfile::certs(&mut file).filter_map(|result| {
                    result
                        .inspect_err(|error| {
                            tracing::error!(
                                %error,
                                ?path,
                                "Failed to parse a file when building a root cert store.",
                            )
                        })
                        .ok()
                });

                let (added, ignored) = root_store.add_parsable_certificates(certs);

                if ignored > 0 {
                    tracing::warn!(
                        ?path,
                        added,
                        "Ignored {ignored} invalid certificate(s) when building a root cert store."
                    );
                }
            }
        }

        root_store
    }
}
