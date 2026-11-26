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
use handler::{IdentityClientConfigs, StealTlsHandler};
use mirrord_agent_env::steal_tls::{
    AgentClientConfig, AgentServerConfig, StealPortTlsConfig, TlsAuthentication, TlsClientIdentity,
    TlsClientVerification, TlsServerVerification,
};
use mirrord_tls_util::{
    CertIdentity, DangerousNoVerifierClient, DangerousNoVerifierServer, FromPemError,
    ParsePemError, add_pem_certs, parse_cert_chain, parse_key_der,
};
use nix::fcntl::OFlag;
use rustls::{
    ClientConfig, RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, ServerName},
    server::{NoClientAuth, WebPkiClientVerifier, danger::ClientCertVerifier},
};
use tracing::Level;

use crate::util::target_root::{TargetRoot, proc_fd_path};

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
    Config(StealPortTlsConfig),
    Handler(StealTlsHandler),
}

/// Inner state of [`StealTlsHandlerStore`].
///
/// Extracted into a separate struct for a nice [`Arc`] wrap.
struct State {
    by_port: Mutex<HashMap<u16, MaybeBuilt>>,
    /// Where the paths in the configs are.
    target_root: TargetRoot,
}

/// Holds [`StealPortTlsConfig`]s for all relevant ports and caches built [`StealTlsHandler`]s.
#[derive(Clone)]
pub struct StealTlsHandlerStore(Arc<State>);

impl StealTlsHandlerStore {
    #[tracing::instrument(level = Level::DEBUG, ret)]
    pub fn new(configs: Vec<StealPortTlsConfig>, target_root: TargetRoot) -> Self {
        let by_port = configs
            .into_iter()
            .map(|config| (config.port, MaybeBuilt::Config(config)))
            .collect();

        Self(Arc::new(State {
            by_port: Mutex::new(by_port),
            target_root,
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

        let server_name = config
            .agent_as_client
            .verification
            .server_name
            .as_deref()
            .map(|name| {
                ServerName::try_from(name)
                    .map(|name| name.to_owned())
                    .map_err(|error| StealTlsSetupErrorInner::InvalidServerName {
                        name: name.to_owned(),
                        error,
                    })
            })
            .transpose()
            .map_err(StealTlsSetupError::ClientSetupError)?;

        // Without verification, the client's certificate proves nothing, and anyone could have
        // the agent present any of the configured identities on their behalf.
        let verifies_clients = config
            .agent_as_server
            .verification
            .as_ref()
            .is_some_and(|verification| verification.accept_any_cert.not());
        if config.agent_as_client.identities.is_empty().not() && verifies_clients.not() {
            return Err(StealTlsSetupError::ClientSetupError(
                StealTlsSetupErrorInner::IdentitiesWithoutClientVerification,
            ));
        }

        let (server_config, (client_config, identity_client_configs)) = tokio::try_join!(
            async {
                self.build_server_config(config.agent_as_server)
                    .await
                    .map_err(StealTlsSetupError::ServerSetupError)
            },
            async {
                self.build_client_configs(config.agent_as_client)
                    .await
                    .map_err(StealTlsSetupError::ClientSetupError)
            },
        )?;

        let handler = StealTlsHandler {
            server_config,
            client_config,
            identity_client_configs,
            verifies_clients,
            server_name,
        };

        let handler_cloned = handler.clone();
        self.0
            .by_port
            .lock()?
            .insert(port, MaybeBuilt::Handler(handler_cloned));

        Ok(Some(handler))
    }

    /// Runs `read` on the target's filesystem, in a blocking task like [`mirrord_tls_util`] reads
    /// its files in.
    async fn in_target<T: Send + 'static>(
        &self,
        read: impl FnOnce(&TargetRoot) -> Result<T, StealTlsSetupErrorInner> + Send + 'static,
    ) -> Result<T, StealTlsSetupErrorInner> {
        let target_root = self.0.target_root.clone();

        tokio::task::spawn_blocking(move || read(&target_root)).await?
    }

    /// Reads the PEM file at `path` in the target's filesystem, and parses it with `parse`.
    fn read_pem<T>(
        target_root: &TargetRoot,
        path: PathBuf,
        parse: impl FnOnce(BufReader<File>) -> Result<T, ParsePemError>,
    ) -> Result<T, FromPemError> {
        match target_root.open_from_root(&path, OFlag::O_RDONLY) {
            Ok(file) => {
                parse(BufReader::new(File::from(file))).map_err(|error| error.in_file(path))
            }
            Err(error) => Err(FromPemError::OpenFileError { error, path }),
        }
    }

    /// Builds a [`RootCertStore`] from the PEM files at `paths` in the target's filesystem, and
    /// from the files directly in the directories among them, like
    /// [`mirrord_tls_util::best_effort_root_store`] does on the agent's own filesystem.
    ///
    /// Logs the files it can't read and skips them.
    fn root_store(target_root: &TargetRoot, paths: Vec<PathBuf>) -> RootCertStore {
        let mut root_store = RootCertStore::empty();

        for path in paths {
            let file = match target_root.open_from_root(&path, OFlag::O_RDONLY) {
                Ok(file) => File::from(file),
                Err(error) => {
                    tracing::error!(
                        %error,
                        ?path,
                        "Failed to open a file when building a root cert store."
                    );
                    continue;
                }
            };

            if file
                .metadata()
                .is_ok_and(|metadata| metadata.is_dir())
                .not()
            {
                add_pem_certs(&mut root_store, BufReader::new(file), &path);
                continue;
            }

            let entries = match std::fs::read_dir(proc_fd_path(&file)) {
                Ok(entries) => entries,
                Err(error) => {
                    tracing::error!(
                        %error,
                        ?path,
                        "Failed to list a directory when building a root cert store."
                    );
                    continue;
                }
            };

            for entry in entries {
                let name = match entry {
                    Ok(entry) => entry.file_name(),
                    Err(error) => {
                        tracing::error!(
                            %error,
                            ?path,
                            "Failed to list a directory when building a root cert store."
                        );
                        continue;
                    }
                };
                let entry_path = path.join(&name);

                let file = match target_root.open_from_root(&entry_path, OFlag::O_RDONLY) {
                    Ok(file) => File::from(file),
                    Err(error) => {
                        tracing::error!(
                            %error,
                            path = ?entry_path,
                            "Failed to open a file when building a root cert store."
                        );
                        continue;
                    }
                };

                if file
                    .metadata()
                    .is_ok_and(|metadata| metadata.is_dir())
                    .not()
                {
                    add_pem_certs(&mut root_store, BufReader::new(file), &entry_path);
                }
            }
        }

        root_store
    }

    /// Builds [`ServerConfig`] for the mirrord-agent's TLS acceptor.
    #[tracing::instrument(level = Level::DEBUG, ret, err(level = Level::DEBUG))] // errors are already logged on `ERROR` level in `get`
    async fn build_server_config(
        &self,
        config: AgentServerConfig,
    ) -> Result<Arc<ServerConfig>, StealTlsSetupErrorInner> {
        let verifier: Arc<dyn ClientCertVerifier> = match config.verification {
            Some(TlsClientVerification {
                allow_anonymous,
                accept_any_cert,
                trust_roots,
            }) => {
                let mut root_store = self
                    .in_target(move |target_root| Ok(Self::root_store(target_root, trust_roots)))
                    .await?;

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

        let (cert_chain, key_der) = self.read_authentication(config.authentication).await?;

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

    /// Builds base [`ClientConfig`]s for the mirrord-agent's TLS connector.
    ///
    /// Returns the default config, and configs presenting [`AgentClientConfig::identities`],
    /// each with the identity of its certificate.
    #[tracing::instrument(level = Level::DEBUG, ret, err(level = Level::DEBUG))] // errors are already logged on `ERROR` level in `get`
    async fn build_client_configs(
        &self,
        config: AgentClientConfig,
    ) -> Result<(Arc<ClientConfig>, IdentityClientConfigs), StealTlsSetupErrorInner> {
        let TlsServerVerification {
            accept_any_cert,
            trust_roots,
            server_name: _,
        } = config.verification;

        let builder = if accept_any_cert {
            ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(DangerousNoVerifierServer))
        } else {
            let root_store = self
                .in_target(move |target_root| Ok(Self::root_store(target_root, trust_roots)))
                .await?;

            if root_store.is_empty() {
                return Err(StealTlsSetupErrorInner::NoGoodRoot);
            }

            ClientConfig::builder().with_root_certificates(root_store)
        };

        let client_config = match config.authentication {
            Some(authentication) => {
                let (cert_chain, key_der) = self.read_authentication(authentication).await?;
                builder
                    .clone()
                    .with_client_auth_cert(cert_chain, key_der)
                    .map_err(StealTlsSetupErrorInner::CertChainInvalid)?
            }
            None => builder.clone().with_no_client_auth(),
        };

        let mut identity_client_configs: IdentityClientConfigs =
            Vec::with_capacity(config.identities.len());
        for TlsClientIdentity { cert, key } in config.identities {
            let path = cert.clone();
            let (cert_chain, key_der) = self
                .read_authentication(TlsAuthentication {
                    cert_pem: cert,
                    key_pem: key,
                })
                .await?;
            let Some(identity) = cert_chain
                .first()
                .and_then(|cert| CertIdentity::from_der(cert))
            else {
                return Err(StealTlsSetupErrorInner::NoCertIdentity(path));
            };
            if identity_client_configs
                .iter()
                .any(|(existing, _)| *existing == identity)
            {
                tracing::warn!(
                    path = %path.display(),
                    "Certificate has the same identity as an earlier one in \
                    `agentAsClient.identities`, the earlier one is used",
                );
            }
            let config = builder
                .clone()
                .with_client_auth_cert(cert_chain, key_der)
                .map_err(StealTlsSetupErrorInner::CertChainInvalid)?;
            identity_client_configs.push((identity, Arc::new(config)));
        }

        Ok((Arc::new(client_config), identity_client_configs))
    }

    /// Reads the certificate chain and the private key from the target container filesystem.
    async fn read_authentication(
        &self,
        TlsAuthentication { cert_pem, key_pem }: TlsAuthentication,
    ) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), StealTlsSetupErrorInner>
    {
        self.in_target(move |target_root| {
            let cert_chain = Self::read_pem(target_root, cert_pem, parse_cert_chain)?;
            let key_der = Self::read_pem(target_root, key_pem, parse_key_der)?;

            Ok((cert_chain, key_der))
        })
        .await
    }

    /// Adds a dummy self-signed certificate to the given [`RootCertStore`].
    ///
    /// Sometimes required in [`Self::build_server_config`].
    #[tracing::instrument(level = Level::DEBUG, ret, err(level = Level::DEBUG))] // errors are already logged on `ERROR` level in `get`
    fn add_dummy(root_store: &mut RootCertStore) -> Result<(), StealTlsSetupErrorInner> {
        let dummy = rcgen::generate_simple_self_signed(vec!["dummy".to_owned()])?;
        let der = CertificateDer::from(dummy.cert);
        root_store
            .add(der)
            .map_err(StealTlsSetupErrorInner::GeneratedInvalidDummy)?;

        Ok(())
    }
}

impl Default for StealTlsHandlerStore {
    fn default() -> Self {
        Self(Arc::new(State {
            by_port: Default::default(),
            // Does not matter, will never be used.
            target_root: TargetRoot::for_target(None),
        }))
    }
}

impl fmt::Debug for StealTlsHandlerStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StealTlsHandlerStore")
            .field("target_root", &self.0.target_root)
            .field("by_port", &self.0.by_port.lock())
            .finish()
    }
}
