//! Builds `kube::Client`s for remote Kubernetes clusters from [`ClusterCredentials`].
//!
//! Only the HTTP/1.1 client is built: it serves both regular requests and WebSocket upgrades.

use std::{
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
use mirrord_nightly_polyfill::error::Report;
use tower::{BoxError, ServiceBuilder};

use crate::{
    credentials::ClusterCredentials,
    error::{ClusterAuthError, Result, context},
    tls::{DefaultTlsBuilder, TlsConfigBuilder},
};

/// Builds clients for remote clusters, with bearer tokens kept in token files the clients
/// re-read, so long-running connections survive token refresh.
pub struct ClusterClientFactory {
    /// Directory for token files (kube::Client reads from these)
    token_files_dir: PathBuf,

    /// TLS configuration builder
    tls_builder: Arc<dyn TlsConfigBuilder>,
}

impl Default for ClusterClientFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl ClusterClientFactory {
    /// Create a new client factory.
    ///
    /// Creates a temporary directory for token files.
    pub fn new() -> Self {
        // Create temp directory for token files
        let token_files_dir = std::env::temp_dir().join("mirrord-cluster-tokens");
        if let Err(e) = std::fs::create_dir_all(&token_files_dir) {
            tracing::warn!(
                dir = %token_files_dir.display(),
                error = %Report::new(&e),
                "Failed to create token files directory"
            );
        }

        Self {
            token_files_dir,
            tls_builder: Arc::new(DefaultTlsBuilder),
        }
    }

    /// Directory the token files of built clients are written to.
    pub fn token_files_dir(&self) -> &Path {
        &self.token_files_dir
    }

    /// Write token to file for kube::Client to read.
    pub fn write_token_file(&self, path: &PathBuf, token: &str) -> Result<()> {
        std::fs::write(path, token).map_err(|e| {
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
        tracing::debug!(
            cluster = %creds.name,
            server = %creds.server,
            "Building client"
        );

        // Write token to file if using bearer token
        // kube::Client will re-read from file on each request
        let token_file = if let Some(ref token) = creds.token {
            let path = self.token_files_dir.join(format!("{}.token", creds.name));
            self.write_token_file(&path, token)?;
            Some(path.to_string_lossy().into_owned())
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
                    // Use token_file so client re-reads on each request (survives refresh)
                    token_file,
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

        let http1_service = ServiceBuilder::new()
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

        Ok(http1_client)
    }
}
