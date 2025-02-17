use std::{
    collections::HashMap,
    fmt,
    fs::File,
    io::BufReader,
    ops::Not,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use error::{StealTlsSetupError, StealTlsSetupErrorInner};
use handler::StealTlsHandler;
use mirrord_agent_env::steal_tls::{
    AgentClientConfig, AgentServerConfig, StealPortTlsConfig, TlsAuthentication,
    TlsClientVerification, TlsServerVerification,
};
use mirrord_tls_util::{
    best_effort_root_store, DangerousNoVerifierClient, DangerousNoVerifierServer,
};
use rustls::{
    pki_types::{CertificateDer, PrivateKeyDer},
    server::{danger::ClientCertVerifier, NoClientAuth, WebPkiClientVerifier},
    ClientConfig, RootCertStore, ServerConfig,
};
use rustls_pemfile::Item;
use tracing::Level;

use crate::util::path_resolver::InTargetPathResolver;

pub mod error;
pub mod handler;

/// Name of HTTP/2 in the ALPN protocol.
pub const HTTP_2_ALPN_NAME: &[u8] = b"h2";
/// Name of HTTP/1.1 in the ALPN protocol.
pub const HTTP_1_1_ALPN_NAME: &[u8] = b"http/1.1";
/// Name of HTTP/1.0 in the ALPN protocol.
pub const HTTP_1_0_ALPN_NAME: &[u8] = b"http/1.0";

/// An already built [`StealTlsHandler`] or a configuration to build one.
#[derive(Debug)]
enum MaybeBuilt {
    Config(StealPortTlsConfig),
    Handler(StealTlsHandler),
}

/// Inner state of [`StealTlsHandlerStore`].
///
/// Extracted into a separate struct for a nice [`Arc`] wrap.
struct State {
    by_port: Mutex<HashMap<u16, MaybeBuilt>>,
    path_resolver: InTargetPathResolver,
}

/// Holds [`StealPortTlsConfig`]s for all relevant ports and caches built [`StealTlsHandler`]s.
#[derive(Clone)]
pub struct StealTlsHandlerStore(Arc<State>);

impl StealTlsHandlerStore {
    #[tracing::instrument(level = Level::DEBUG, ret)]
    pub fn new(config: Vec<StealPortTlsConfig>, path_resolver: InTargetPathResolver) -> Self {
        let by_port = config
            .into_iter()
            .map(|config| (config.port, MaybeBuilt::Config(config)))
            .collect();

        Self(Arc::new(State {
            by_port: Mutex::new(by_port),
            path_resolver,
        }))
    }

    /// Reuses or builds a [`StealTlsHandler`] for the given port.
    ///
    /// Returns [`None`] if this port is not covered by the TLS steal config.
    #[tracing::instrument(level = Level::DEBUG, ret, err)]
    pub async fn get(&self, port: u16) -> Result<Option<StealTlsHandler>, StealTlsSetupError> {
        let config = match self.0.by_port.lock()?.get(&port) {
            None => return Ok(None),
            Some(MaybeBuilt::Handler(handler)) => return Ok(Some(handler.clone())),
            Some(MaybeBuilt::Config(config)) => config.clone(),
        };

        let this = self.clone();
        let handler = tokio::task::spawn_blocking(move || {
            let server_config = this
                .build_server_config(config.agent_as_server)
                .map_err(StealTlsSetupError::ServerSetupError)?;
            let client_config = this
                .build_client_config(config.agent_as_client)
                .map_err(StealTlsSetupError::ClientSetupError)?;

            Ok::<_, StealTlsSetupError>(StealTlsHandler {
                server_config,
                client_config,
            })
        })
        .await??;

        let handler_cloned = handler.clone();
        self.0
            .by_port
            .lock()?
            .insert(port, MaybeBuilt::Handler(handler_cloned));

        Ok(Some(handler))
    }

    /// Resolves the given path in the target container filesystem.
    #[tracing::instrument(level = Level::DEBUG, ret, err(level = Level::DEBUG))] // errors are already logged on `ERROR` level in `get`
    fn resolve_path(&self, path: PathBuf) -> Result<PathBuf, StealTlsSetupErrorInner> {
        self.0
            .path_resolver
            .resolve(&path)
            .map_err(|error| StealTlsSetupErrorInner::PathResolutionError { error, path })
    }

    /// Builds [`ServerConfig`] for the mirrord-agent's TLS acceptor.
    #[tracing::instrument(level = Level::DEBUG, ret, err(level = Level::DEBUG))] // errors are already logged on `ERROR` level in `get`
    fn build_server_config(
        &self,
        config: AgentServerConfig,
    ) -> Result<Arc<ServerConfig>, StealTlsSetupErrorInner> {
        let verifier: Arc<dyn ClientCertVerifier> = match config.verification {
            Some(TlsClientVerification {
                allow_anonymous,
                accept_any_cert,
                trust_roots,
            }) => {
                let trust_roots = trust_roots
                    .into_iter()
                    .map(|root| self.resolve_path(root))
                    .collect::<Result<Vec<_>, _>>()?;
                let mut root_store = best_effort_root_store(trust_roots);

                if root_store.is_empty() && accept_any_cert.not() {
                    if allow_anonymous {
                        // `WebPkiClientVerifier` requires at least one trust anchor.
                        // Since we want anonymous clients to be able to connect,
                        // we insert a self-signed dummy certificate.
                        Self::add_dummy(&mut root_store)?;
                    } else {
                        return Err(StealTlsSetupErrorInner::NoGoodRoot);
                    }
                }

                if accept_any_cert {
                    Arc::new(DangerousNoVerifierClient {
                        allow_anonymous,
                        subjects: root_store.subjects(),
                    })
                } else {
                    let mut builder = WebPkiClientVerifier::builder(root_store.into());
                    if allow_anonymous {
                        builder = builder.allow_unauthenticated();
                    }
                    builder.build().map_err(StealTlsSetupErrorInner::from)?
                }
            }
            None => Arc::new(NoClientAuth),
        };

        let TlsAuthentication { cert_pem, key_pem } = config.authentication;
        let cert_chain = self.read_cert_chain(cert_pem)?;
        let key_der = self.read_key_der(key_pem)?;

        let mut server_config = ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(cert_chain, key_der)
            .map_err(StealTlsSetupErrorInner::CertChainInvalid)?;

        server_config.alpn_protocols = config
            .alpn_protocols
            .into_iter()
            .map(String::into_bytes)
            .collect();

        Ok(Arc::new(server_config))
    }

    /// Builds base [`ClientConfig`] for the mirrord-agent's TLS connector.
    #[tracing::instrument(level = Level::DEBUG, ret, err(level = Level::DEBUG))] // errors are already logged on `ERROR` level in `get`
    fn build_client_config(
        &self,
        config: AgentClientConfig,
    ) -> Result<Arc<ClientConfig>, StealTlsSetupErrorInner> {
        let TlsServerVerification {
            accept_any_cert,
            trust_roots,
        } = config.verification;

        let builder = if accept_any_cert {
            ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(DangerousNoVerifierServer))
        } else {
            let trust_roots = trust_roots
                .into_iter()
                .map(|root| self.resolve_path(root))
                .collect::<Result<Vec<_>, _>>()?;
            let root_store = best_effort_root_store(trust_roots);

            if root_store.is_empty() {
                return Err(StealTlsSetupErrorInner::NoGoodRoot);
            }

            ClientConfig::builder().with_root_certificates(root_store)
        };

        let client_config = match config.authentication {
            Some(TlsAuthentication { cert_pem, key_pem }) => {
                let cert_chain = self.read_cert_chain(cert_pem)?;
                let key_der = self.read_key_der(key_pem)?;

                builder
                    .with_client_auth_cert(cert_chain, key_der)
                    .map_err(StealTlsSetupErrorInner::CertChainInvalid)?
            }
            None => builder.with_no_client_auth(),
        };

        Ok(Arc::new(client_config))
    }

    /// Adds a dummy self-signed certificate to the given [`RootCertStore`].
    ///
    /// Sometimes required in [`Self::build_server_config`].
    #[tracing::instrument(level = Level::DEBUG, ret, err(level = Level::DEBUG))] // errors are already logged on `ERROR` level in `get`
    fn add_dummy(root_store: &mut RootCertStore) -> Result<(), StealTlsSetupErrorInner> {
        let dummy = rcgen::generate_simple_self_signed(vec!["dummy".to_string()])?;
        let der = CertificateDer::from(dummy.cert);
        root_store
            .add(der)
            .map_err(StealTlsSetupErrorInner::GeneratedInvalidDummy)?;

        Ok(())
    }

    /// Reads a certificate chain from the given PEM file.
    ///
    /// PEM items of other types are ignored.
    /// At least one certificate is required.
    #[tracing::instrument(level = Level::DEBUG, ret, err(level = Level::DEBUG))] // errors are already logged on `ERROR` level in `get`
    fn read_cert_chain(
        &self,
        path: PathBuf,
    ) -> Result<Vec<CertificateDer<'static>>, StealTlsSetupErrorInner> {
        let path = self.resolve_path(path)?;

        let mut file = match File::open(&path) {
            Ok(file) => BufReader::new(file),
            Err(error) => return Err(StealTlsSetupErrorInner::ParsePemError { error, path }),
        };

        let cert_chain = rustls_pemfile::certs(&mut file).collect::<Result<Vec<_>, _>>();

        match cert_chain {
            Ok(cert_chain) if cert_chain.is_empty().not() => Ok(cert_chain),
            Ok(..) => Err(StealTlsSetupErrorInner::NoCertFound(path)),
            Err(error) => Err(StealTlsSetupErrorInner::ParsePemError { error, path }),
        }
    }

    /// Reads a private key from the given PEM file.
    ///
    /// PEM items of other types are ignored.
    /// Exactly one private key is required.
    #[tracing::instrument(level = Level::DEBUG, ret, err(level = Level::DEBUG))] // errors are already logged on `ERROR` level in `get`
    fn read_key_der(
        &self,
        path: PathBuf,
    ) -> Result<PrivateKeyDer<'static>, StealTlsSetupErrorInner> {
        let path = self.resolve_path(path)?;

        let mut file = match File::open(&path) {
            Ok(file) => BufReader::new(file),
            Err(error) => return Err(StealTlsSetupErrorInner::ParsePemError { error, path }),
        };

        let mut found_key = None;

        for entry in rustls_pemfile::read_all(&mut file) {
            let key = match entry {
                Ok(Item::Pkcs1Key(key)) => PrivateKeyDer::Pkcs1(key),
                Ok(Item::Pkcs8Key(key)) => PrivateKeyDer::Pkcs8(key),
                Ok(Item::Sec1Key(key)) => PrivateKeyDer::Sec1(key),
                Ok(..) => continue,
                Err(error) => return Err(StealTlsSetupErrorInner::ParsePemError { error, path }),
            };

            if found_key.replace(key).is_some() {
                return Err(StealTlsSetupErrorInner::MultipleKeysFound(path));
            }
        }

        found_key.ok_or(StealTlsSetupErrorInner::NoKeyFound(path))
    }
}

impl fmt::Debug for StealTlsHandlerStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StealTlsHandlerStore")
            .field("path_resolver", &self.0.path_resolver)
            .field("by_port", &self.0.by_port.lock())
            .finish()
    }
}
