//! A connection to one remote cluster, and the refresh of its bearer token.

use std::{path::PathBuf, time::Duration};

use kube::Client;
use mirrord_nightly_polyfill::error::Report;
use tokio_retry::strategy::ExponentialBackoff;

use crate::{
    client::ClusterClientFactory,
    credentials::{AuthMethod, ClusterCredentials},
    error::{ClusterAuthError, Result},
    iam_token,
};

/// Active connection to a remote cluster.
#[derive(Clone)]
pub struct ClusterConnection {
    /// Cluster credentials
    pub credentials: ClusterCredentials,

    /// HTTP/1.1 Kubernetes client for this cluster, usable for WebSocket connections.
    pub client: Client,

    /// Token expiration time.
    /// None if using mTLS without bearer token.
    pub token_expiry: Option<std::time::SystemTime>,

    /// Path to token file (for token refresh).
    /// None if using mTLS without bearer token.
    pub token_file_path: Option<PathBuf>,
}

impl ClusterClientFactory {
    /// Load a cluster connection for EKS IAM authentication.
    ///
    /// Generates an initial EKS token using IAM credentials (no Secret needed).
    /// `credentials` must use [`AuthMethod::AwsIam`], whose `cluster_name` is signed into the
    /// token. Unavailable credentials get up to five retries with exponential backoff; dropping
    /// the future cancels both credential resolution and retry sleeps.
    pub async fn connect_eks(
        &self,
        mut credentials: ClusterCredentials,
    ) -> Result<ClusterConnection> {
        let AuthMethod::AwsIam {
            region,
            cluster_name,
        } = &credentials.auth_method
        else {
            return Err(ClusterAuthError::ConfigError(
                format!(
                    "Cluster '{}' does not use AWS IAM authentication",
                    credentials.name
                )
                .into(),
            ));
        };
        let region = region.clone();

        let mut retries = ExponentialBackoff::from_millis(2)
            .factor(500)
            .max_delay(Duration::from_secs(30))
            .take(5);
        let token = loop {
            match iam_token::generate_eks_token(&region, cluster_name).await {
                Ok(token) => break token,
                Err(error @ ClusterAuthError::CredentialsUnavailable(..)) => {
                    let Some(delay) = retries.next() else {
                        return Err(error);
                    };
                    tracing::warn!(
                        error = %Report::new(&error),
                        retry_in_secs = delay.as_secs(),
                        "Initial AWS credential resolution failed, will retry"
                    );
                    tokio::time::sleep(delay).await;
                }
                Err(error) => return Err(error),
            }
        };
        let token_expiry = Some(token.expires_at);
        credentials.token = Some(token.token);

        let (client, token_file_path) = self.build_client_with_token_file(&credentials).await?;

        tracing::debug!(
            cluster = %credentials.name,
            region = %region,
            "Initial EKS IAM token generated"
        );

        Ok(ClusterConnection {
            credentials,
            client,
            token_expiry,
            token_file_path,
        })
    }

    /// Refresh the token of `connection`.
    ///
    /// For IAM clusters: generates a new EKS token locally (no network call).
    /// Other token-based methods are not refreshed here and fail with
    /// [`ClusterAuthError::ConfigError`].
    pub async fn refresh_token(&self, connection: &mut ClusterConnection) -> Result<()> {
        let cluster_name = connection.credentials.name.clone();

        match &connection.credentials.auth_method {
            AuthMethod::AwsIam {
                region,
                cluster_name: eks_cluster_name,
            } => {
                // IAM: generate a new EKS token locally (no network call to the remote cluster)
                tracing::debug!(
                    cluster = %cluster_name,
                    eks_cluster = %eks_cluster_name,
                    "Refreshing IAM token"
                );

                let new_token = iam_token::generate_eks_token(region, eks_cluster_name).await?;
                let new_expiry = Some(new_token.expires_at);

                if let Some(token_file_path) = &connection.token_file_path {
                    self.write_token_file(token_file_path, &new_token.token)?;
                }
                connection.credentials.token = Some(new_token.token);
                connection.token_expiry = new_expiry;

                tracing::info!(cluster = %cluster_name, "IAM token refreshed");
            }
            AuthMethod::Mtls => {
                tracing::debug!(cluster = %cluster_name, "mTLS-only, skipping token refresh");
            }
            auth_method @ (AuthMethod::BearerToken | AuthMethod::AzureWorkloadIdentity { .. }) => {
                return Err(ClusterAuthError::ConfigError(
                    format!("Token refresh is not supported for {auth_method:?} authentication")
                        .into(),
                ));
            }
        }

        Ok(())
    }

    /// Keep the token of `connection` fresh, for as long as the returned future runs.
    ///
    /// Refreshes the token before it expires, and retries a failed refresh with exponential
    /// backoff for unavailable AWS credentials. Other errors reach the caller so it can stop
    /// serving work instead of silently allowing authentication to expire.
    pub async fn run_token_refresh(&self, mut connection: ClusterConnection) -> Result<()> {
        let cluster_name = connection.credentials.name.clone();
        let mut backoff: Option<ExponentialBackoff> = None;

        loop {
            let next_check = match (&connection.credentials.auth_method, connection.token_expiry) {
                (AuthMethod::AwsIam { .. }, Some(expiry)) => {
                    // IAM tokens: check expiry from tracked time
                    if iam_token::needs_refresh(expiry) {
                        tracing::debug!(
                            cluster = %cluster_name,
                            "IAM token expiring, refreshing"
                        );
                        None
                    } else {
                        iam_token::time_until_refresh(expiry)
                    }
                }
                _ => {
                    tracing::debug!(cluster = %cluster_name, "No token to refresh");
                    return Ok(());
                }
            };

            if let Some(next_check) = next_check {
                tracing::debug!(
                    next_check_mins = next_check.as_secs() / 60,
                    "Next token check in {} min",
                    next_check.as_secs() / 60
                );

                tokio::time::sleep(next_check).await;
                continue;
            }

            match self.refresh_token(&mut connection).await {
                Ok(()) => backoff = None,
                Err(e @ ClusterAuthError::CredentialsUnavailable(..)) => {
                    let backoff = backoff.get_or_insert_with(|| {
                        ExponentialBackoff::from_millis(2)
                            .factor(500)
                            .max_delay(Duration::from_secs(30))
                    });
                    let delay = backoff.next().unwrap_or(Duration::from_secs(30));

                    tracing::warn!(
                        cluster = %cluster_name,
                        error = %Report::new(&e),
                        retry_in_secs = delay.as_secs(),
                        "Token refresh failed, will retry"
                    );

                    tokio::time::sleep(delay).await;
                }
                Err(error) => return Err(error),
            }
        }
    }
}
