#![allow(unused)]

use std::{
    collections::HashMap,
    fmt, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use mirrord_agent_env::steal_tls::{StealPortTlsConfig, StealTlsConfig};
use rustls::{
    pki_types::{CertificateDer, PrivateKeyDer},
    ServerConfig,
};
use rustls_pemfile::Item;
use thiserror::Error;
use tokio::fs;
use tokio_rustls::TlsAcceptor;

use crate::file::RootPath;

#[derive(Error, Debug)]
pub enum StealTlsError {
    #[error("failed to read the PEM file at {file}: {error}")]
    ReadPemError {
        #[source]
        error: io::Error,
        file: PathBuf,
    },

    #[error("failed to parse the PEM file at {file}: {error}")]
    ParseError {
        #[source]
        error: io::Error,
        file: PathBuf,
    },

    #[error("no certificate was found in the PEM file at {0}")]
    CertChainEmpty(PathBuf),

    #[error("no private key was found in the PEM file at {0}")]
    KeyNotFound(PathBuf),

    #[error("multiple private keys were found in the PEM file at {0}")]
    MultipleKeysFound(PathBuf),

    #[error("failed to build server config: {0}")]
    BuildConfigError(#[from] rustls::Error),
}

#[derive(Default)]
struct State {
    config: StealTlsConfig,
    resolved: Mutex<HashMap<u16, (TlsAcceptor, Instant)>>,
    root_path: RootPath,
}

#[derive(Clone, Default)]
pub(crate) struct StealTlsAcceptors(Arc<State>);

impl StealTlsAcceptors {
    const ACCEPTOR_VALIDITY: Duration = Duration::from_secs(30);

    pub(crate) fn new(config: StealTlsConfig, target_pid: u64) -> Self {
        Self(Arc::new(State {
            config,
            resolved: Default::default(),
            root_path: RootPath::new(target_pid),
        }))
    }

    pub(crate) async fn get_acceptor(
        &self,
        port: u16,
    ) -> Result<Option<TlsAcceptor>, StealTlsError> {
        let ready = self
            .0
            .resolved
            .lock()
            .inspect_err(|_| tracing::error!("TLS acceptors mutex is poisoned"))
            .ok()
            .and_then(|resolved| resolved.get(&port).cloned())
            .filter(|entry| entry.1.elapsed() < Self::ACCEPTOR_VALIDITY);
        if let Some(ready) = ready {
            return Ok(Some(ready.0.clone()));
        }

        let Some(config) = self.0.config.get(&port) else {
            return Ok(None);
        };

        let acceptor = self.build_acceptor(config).await?;

        if let Ok(mut resolved) = self.0.resolved.lock() {
            resolved.insert(port, (acceptor.clone(), Instant::now()));
        } else {
            tracing::error!("TLS acceptors mutex is poisoned");
        }

        Ok(Some(acceptor))
    }

    async fn build_acceptor(
        &self,
        config: &StealPortTlsConfig,
    ) -> Result<TlsAcceptor, StealTlsError> {
        let cert_chain = self.get_cert_chain(&config.server_cert_pem).await?;
        let key = self.get_key(&config.server_key_pem).await?;

        let mut server_config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(cert_chain, key)?;

        server_config.alpn_protocols = config
            .alpn_protocols
            .iter()
            .map(ToString::to_string)
            .map(String::into_bytes)
            .map(Vec::from)
            .collect();

        Ok(TlsAcceptor::from(Arc::new(server_config)))
    }

    async fn get_cert_chain(
        &self,
        path: &Path,
    ) -> Result<Vec<CertificateDer<'static>>, StealTlsError> {
        let pem: io::Result<Vec<u8>> = try {
            let path = self.0.root_path.resolve_path(path)?;
            fs::read(path).await?
        };

        let pem = pem.map_err(|error| StealTlsError::ReadPemError {
            error,
            file: path.to_path_buf(),
        })?;

        let cert_chain = rustls_pemfile::certs(&mut pem.as_slice())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| StealTlsError::ParseError {
                error,
                file: path.to_path_buf(),
            })?;

        if cert_chain.is_empty() {
            return Err(StealTlsError::CertChainEmpty(path.to_path_buf()));
        }

        Ok(cert_chain)
    }

    async fn get_key(&self, path: &Path) -> Result<PrivateKeyDer<'static>, StealTlsError> {
        let pem: io::Result<Vec<u8>> = try {
            let path = self.0.root_path.resolve_path(path)?;
            fs::read(path).await?
        };

        let pem = pem.map_err(|error| StealTlsError::ReadPemError {
            error,
            file: path.to_path_buf(),
        })?;

        let mut found_key = None;

        for key in rustls_pemfile::read_all(&mut pem.as_slice()) {
            let key = match key {
                Err(error) => {
                    return Err(StealTlsError::ParseError {
                        error,
                        file: path.to_path_buf(),
                    });
                }
                Ok(Item::Pkcs1Key(key)) => PrivateKeyDer::Pkcs1(key),
                Ok(Item::Pkcs8Key(key)) => PrivateKeyDer::Pkcs8(key),
                Ok(Item::Sec1Key(key)) => PrivateKeyDer::Sec1(key),
                _ => continue,
            };

            if found_key.replace(key).is_some() {
                return Err(StealTlsError::MultipleKeysFound(path.to_path_buf()));
            }
        }

        found_key.ok_or(StealTlsError::KeyNotFound(path.to_path_buf()))
    }
}

impl fmt::Debug for StealTlsAcceptors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StealTlsAcceptors")
            .field("config", &self.0.config)
            .field("root_path", &self.0.root_path)
            .field(
                "resolved",
                &self.0.resolved.lock().as_ref().map(|r| r.keys()),
            )
            .finish()
    }
}
