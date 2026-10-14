//! Builds `kube::Client`s for remote Kubernetes clusters from [`ClusterCredentials`].
//!
//! Only the HTTP/1.1 client is built: it serves both regular requests and WebSocket upgrades.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use hyper_util::rt::TokioExecutor;
use kube::{
    Client, Config,
    client::ConfigExt,
    config::{
        AuthInfo, Cluster, Context, KubeConfigOptions, Kubeconfig, NamedAuthInfo, NamedCluster,
        NamedContext,
    },
};
use tempfile::{NamedTempFile, TempDir};
use tower::{BoxError, ServiceBuilder};

use crate::{
    credentials::ClusterCredentials,
    error::{ClusterAuthError, Result, context},
    tls::{DefaultTlsBuilder, TlsConfigBuilder},
};

/// Builds clients with private token files that kube reloads at least once per minute.
/// The clients retain the temporary directory even after the factory is dropped.
pub struct ClusterClientFactory {
    /// Directory for token files (kube::Client reads from these)
    token_files_dir: Arc<TempDir>,

    /// TLS configuration builder
    tls_builder: Arc<dyn TlsConfigBuilder>,
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn token_replacement_does_not_follow_existing_symlinks() {
        let factory = ClusterClientFactory::new().unwrap();
        let other_factory = ClusterClientFactory::new().unwrap();
        assert_ne!(factory.token_files_dir(), other_factory.token_files_dir());
        let mut target = NamedTempFile::new().unwrap();
        target.write_all(b"untouched").unwrap();
        let token_path = factory.token_files_dir().join("token");
        symlink(target.path(), &token_path).unwrap();

        factory
            .write_token_file(&token_path, "first-token")
            .unwrap();
        assert_eq!(std::fs::read_to_string(target.path()).unwrap(), "untouched");
        assert!(!std::fs::symlink_metadata(&token_path).unwrap().is_symlink());
        factory
            .write_token_file(&token_path, "replacement")
            .unwrap();
        assert_eq!(std::fs::read_to_string(&token_path).unwrap(), "replacement");
        assert_eq!(
            std::fs::metadata(&token_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

impl ClusterClientFactory {
    /// Create a new client factory.
    ///
    /// Creates a temporary directory for token files.
    pub fn new() -> Result<Self> {
        let mut builder = tempfile::Builder::new();
        builder.prefix("mirrord-cluster-tokens-");
        #[cfg(unix)]
        builder.permissions(std::fs::Permissions::from_mode(0o700));
        let token_files_dir = builder.tempdir().map_err(|error| {
            ClusterAuthError::Internal(context("Failed to create private token directory", error))
        })?;

        Ok(Self {
            token_files_dir: Arc::new(token_files_dir),
            tls_builder: Arc::new(DefaultTlsBuilder),
        })
    }

    /// Directory the token files of built clients are written to.
    pub fn token_files_dir(&self) -> &Path {
        self.token_files_dir.path()
    }

    /// Atomically replaces a token so a concurrent kube reload cannot read a partial write.
    pub(crate) fn write_token_file(&self, path: &Path, token: &str) -> Result<()> {
        let write = || -> std::io::Result<()> {
            let mut file = NamedTempFile::new_in(self.token_files_dir())?;
            file.write_all(token.as_bytes())?;
            file.persist(path).map_err(|error| error.error)?;
            Ok(())
        };
        write().map_err(|e| {
            ClusterAuthError::Internal(context(
                format!("Failed to write token file {}", path.display()),
                e,
            ))
        })
    }

    /// Build an HTTP/1.1 kube::Client from cluster credentials, usable for WebSocket connections.
    ///
    /// If using bearer token, writes token to file and configures client to read from it.
    /// This allows long-running connections to survive token refresh.
    pub async fn build_client(&self, creds: &ClusterCredentials) -> Result<Client> {
        self.build_client_with_token_file(creds)
            .await
            .map(|(client, _)| client)
    }

    pub(crate) async fn build_client_with_token_file(
        &self,
        creds: &ClusterCredentials,
    ) -> Result<(Client, Option<PathBuf>)> {
        tracing::debug!(
            cluster = %creds.name,
            server = %creds.server,
            "Building client"
        );

        let token_file_path = if let Some(ref token) = creds.token {
            let file = NamedTempFile::new_in(self.token_files_dir()).map_err(|error| {
                ClusterAuthError::Internal(context("Failed to create private token file", error))
            })?;
            let path = file.into_temp_path().keep().map_err(|error| {
                ClusterAuthError::Internal(context("Failed to retain token file", error))
            })?;
            self.write_token_file(&path, token)?;
            Some(path)
        } else {
            None
        };

        let kubeconfig = Kubeconfig {
            clusters: vec![NamedCluster {
                name: creds.name.clone(),
                cluster: Some(Cluster {
                    server: Some(creds.server.clone()),
                    certificate_authority_data: creds.ca_data.clone(),
                    ..Default::default()
                }),
                other: Default::default(),
            }],
            auth_infos: vec![NamedAuthInfo {
                name: creds.name.clone(),
                auth_info: Some(AuthInfo {
                    token_file: token_file_path
                        .as_ref()
                        .map(|path| path.to_string_lossy().into_owned()),
                    ..Default::default()
                }),
                other: Default::default(),
            }],
            contexts: vec![NamedContext {
                name: creds.name.clone(),
                context: Some(Context {
                    cluster: creds.name.clone(),
                    user: Some(creds.name.clone()),
                    namespace: Some(creds.namespace.clone()),
                    ..Default::default()
                }),
                other: Default::default(),
            }],
            current_context: Some(creds.name.clone()),
            ..Default::default()
        };

        let config = Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default())
            .await
            .map_err(|e| ClusterAuthError::RemoteClusterConnection {
                cluster: creds.name.clone(),
                source: Box::new(e),
            })?;

        // Build TLS config
        let tls_config = self.tls_builder.build(creds)?;

        // HTTP/1.1-only client for WebSocket connections.
        // WebSocket upgrade mechanism uses Connection: Upgrade header which only works with
        // HTTP/1.1.
        let http1_connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls_config)
            .https_only()
            .enable_http1()
            .build();

        let token_files_dir = self.token_files_dir.clone();
        let http1_service = ServiceBuilder::new()
            .map_request(move |request| {
                let _keep_alive = &token_files_dir;
                request
            })
            .layer(config.base_uri_layer())
            .option_layer(config.auth_layer().map_err(|e| {
                ClusterAuthError::RemoteClusterConnection {
                    cluster: creds.name.clone(),
                    source: Box::new(e),
                }
            })?)
            .map_err(BoxError::from)
            .service(
                hyper_util::client::legacy::Client::builder(TokioExecutor::new())
                    .build(http1_connector),
            );

        let http1_client = Client::new(http1_service, config.default_namespace);

        Ok((http1_client, token_file_path))
    }
}
