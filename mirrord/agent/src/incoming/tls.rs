use std::{
    collections::HashMap,
    fmt,
    ops::Not,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use error::{IncomingTlsSetupError, IncomingTlsSetupErrorInner};
use handler::IncomingTlsHandler;
use mirrord_agent_env::steal_tls::{
    AgentClientConfig, AgentServerConfig, IncomingPortTlsConfig, TlsAuthentication,
    TlsClientVerification, TlsServerVerification,
};
use mirrord_tls_util::{
    best_effort_root_store, DangerousNoVerifierClient, DangerousNoVerifierServer,
};
use rustls::{
    pki_types::CertificateDer,
    server::{danger::ClientCertVerifier, NoClientAuth, WebPkiClientVerifier},
    ClientConfig, RootCertStore, ServerConfig,
};
use tracing::Level;

use crate::util::path_resolver::InTargetPathResolver;

pub mod error;
pub mod handler;
#[cfg(test)]
pub mod test;

/// Name of HTTP/2 in the ALPN protocol.
pub const HTTP_2_ALPN_NAME: &[u8] = b"h2";
/// Name of HTTP/1.1 in the ALPN protocol.
pub const HTTP_1_1_ALPN_NAME: &[u8] = b"http/1.1";
/// Name of HTTP/1.0 in the ALPN protocol.
pub const HTTP_1_0_ALPN_NAME: &[u8] = b"http/1.0";

/// An already built [`StealTlsHandler`] or a configuration to build one.
#[derive(Debug)]
enum MaybeBuilt {
    Config(IncomingPortTlsConfig),
    Handler(IncomingTlsHandler),
}

/// Inner state of [`StealTlsHandlerStore`].
///
/// Extracted into a separate struct for a nice [`Arc`] wrap.
struct State {
    by_port: Mutex<HashMap<u16, MaybeBuilt>>,
    path_resolver: InTargetPathResolver,
}

/// Holds [`IncomingPortTlsConfig`]s for all relevant ports and caches built [`StealTlsHandler`]s.
#[derive(Clone)]
pub struct IncomingTlsHandlerStore(Arc<State>);

impl IncomingTlsHandlerStore {
    #[tracing::instrument(level = Level::DEBUG, ret)]
    pub fn new(configs: Vec<IncomingPortTlsConfig>, path_resolver: InTargetPathResolver) -> Self {
        let by_port = configs
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
    pub async fn get(
        &self,
        port: u16,
    ) -> Result<Option<IncomingTlsHandler>, IncomingTlsSetupError> {
        let config = match self.0.by_port.lock()?.get(&port) {
            None => return Ok(None),
            Some(MaybeBuilt::Handler(handler)) => return Ok(Some(handler.clone())),
            Some(MaybeBuilt::Config(config)) => config.clone(),
        };

        let (server_config, client_config) = tokio::try_join!(
            async {
                self.build_server_config(config.agent_as_server)
                    .await
                    .map_err(IncomingTlsSetupError::ServerSetupError)
            },
            async {
                self.build_client_config(config.agent_as_client)
                    .await
                    .map_err(IncomingTlsSetupError::ClientSetupError)
            },
        )?;

        let handler = IncomingTlsHandler {
            server_config,
            client_config,
        };

        let handler_cloned = handler.clone();
        self.0
            .by_port
            .lock()?
            .insert(port, MaybeBuilt::Handler(handler_cloned));

        Ok(Some(handler))
    }

    /// Resolves the given path in the target container filesystem.
    #[tracing::instrument(level = Level::DEBUG, ret, err(level = Level::DEBUG))] // errors are already logged on `ERROR` level in `get`
    fn resolve_path(&self, path: PathBuf) -> Result<PathBuf, IncomingTlsSetupErrorInner> {
        self.0
            .path_resolver
            .resolve(&path)
            .map_err(|error| IncomingTlsSetupErrorInner::PathResolutionError { error, path })
    }

    /// Builds [`ServerConfig`] for the mirrord-agent's TLS acceptor.
    #[tracing::instrument(level = Level::DEBUG, ret, err(level = Level::DEBUG))] // errors are already logged on `ERROR` level in `get`
    async fn build_server_config(
        &self,
        config: AgentServerConfig,
    ) -> Result<Arc<ServerConfig>, IncomingTlsSetupErrorInner> {
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
                let mut root_store = best_effort_root_store(trust_roots).await?;

                if root_store.is_empty() && accept_any_cert.not() {
                    if allow_anonymous {
                        // `WebPkiClientVerifier` requires at least one trust anchor.
                        // Since we want anonymous clients to be able to connect,
                        // we insert a self-signed dummy certificate.
                        Self::add_dummy(&mut root_store)?;
                    } else {
                        return Err(IncomingTlsSetupErrorInner::NoGoodRoot);
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
                    builder.build().map_err(IncomingTlsSetupErrorInner::from)?
                }
            }
            None => Arc::new(NoClientAuth),
        };

        let TlsAuthentication { cert_pem, key_pem } = config.authentication;
        let cert_chain = {
            let path = self.resolve_path(cert_pem)?;
            mirrord_tls_util::read_cert_chain(path).await?
        };
        let key_der = {
            let path = self.resolve_path(key_pem)?;
            mirrord_tls_util::read_key_der(path).await?
        };

        let mut server_config = ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(cert_chain, key_der)
            .map_err(IncomingTlsSetupErrorInner::CertChainInvalid)?;

        server_config.alpn_protocols = config
            .alpn_protocols
            .into_iter()
            .map(String::into_bytes)
            .collect();

        Ok(Arc::new(server_config))
    }

    /// Builds base [`ClientConfig`] for the mirrord-agent's TLS connector.
    #[tracing::instrument(level = Level::DEBUG, ret, err(level = Level::DEBUG))] // errors are already logged on `ERROR` level in `get`
    async fn build_client_config(
        &self,
        config: AgentClientConfig,
    ) -> Result<Arc<ClientConfig>, IncomingTlsSetupErrorInner> {
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
            let root_store = best_effort_root_store(trust_roots).await?;

            if root_store.is_empty() {
                return Err(IncomingTlsSetupErrorInner::NoGoodRoot);
            }

            ClientConfig::builder().with_root_certificates(root_store)
        };

        let client_config = match config.authentication {
            Some(TlsAuthentication { cert_pem, key_pem }) => {
                let cert_chain = {
                    let path = self.resolve_path(cert_pem)?;
                    mirrord_tls_util::read_cert_chain(path).await?
                };
                let key_der = {
                    let path = self.resolve_path(key_pem)?;
                    mirrord_tls_util::read_key_der(path).await?
                };

                builder
                    .with_client_auth_cert(cert_chain, key_der)
                    .map_err(IncomingTlsSetupErrorInner::CertChainInvalid)?
            }
            None => builder.with_no_client_auth(),
        };

        Ok(Arc::new(client_config))
    }

    /// Adds a dummy self-signed certificate to the given [`RootCertStore`].
    ///
    /// Sometimes required in [`Self::build_server_config`].
    #[tracing::instrument(level = Level::DEBUG, ret, err(level = Level::DEBUG))] // errors are already logged on `ERROR` level in `get`
    fn add_dummy(root_store: &mut RootCertStore) -> Result<(), IncomingTlsSetupErrorInner> {
        let dummy = rcgen::generate_simple_self_signed(vec!["dummy".to_string()])?;
        let der = CertificateDer::from(dummy.cert);
        root_store
            .add(der)
            .map_err(IncomingTlsSetupErrorInner::GeneratedInvalidDummy)?;

        Ok(())
    }
}

impl fmt::Debug for IncomingTlsHandlerStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StealTlsHandlerStore")
            .field("path_resolver", &self.0.path_resolver)
            .field("by_port", &self.0.by_port.lock())
            .finish()
    }
}

#[cfg(test)]
impl IncomingTlsHandlerStore {
    pub fn dummy() -> Self {
        Self::new(
            Default::default(),
            InTargetPathResolver::with_root_path("/".into()),
        )
    }
}
