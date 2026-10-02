//! AWS EKS IAM token generation.
//!
//! Generates short-lived tokens for authenticating to EKS clusters using IAM roles.
//! The token is a presigned STS `GetCallerIdentity` URL, base64url-encoded and
//! prefixed with `k8s-aws-v1.`.
//!
//! The EKS API server validates the token by calling STS with the presigned URL,
//! then maps the IAM identity to a Kubernetes user/group via an EKS Access Entry.
//!
//! Token lifetime is **15 minutes** (AWS maximum for STS presigned URLs).
//!
//! # How it works
//!
//! 1. We construct a presigned STS `GetCallerIdentity` URL with a `x-k8s-aws-id` signed header (set
//!    to the EKS cluster name).
//! 2. The URL is base64url-encoded and prefixed with `k8s-aws-v1.`.
//! 3. This token is used as a standard bearer token in Kubernetes API requests.
//! 4. The EKS authenticator webhook decodes the token, calls STS with the presigned URL (including
//!    the `x-k8s-aws-id` header matching its own cluster), and if STS validates the signature, the
//!    caller's IAM identity is returned.
//! 5. The IAM identity is then mapped to a Kubernetes user/group via an EKS Access Entry.
//!
//! # Reference
//! - AWS CLI implementation: <https://github.com/aws/aws-cli/blob/develop/awscli/customizations/eks/get_token.py>
//! - EKS cluster authentication: <https://docs.aws.amazon.com/eks/latest/userguide/cluster-auth.html>

use std::time::{Duration, SystemTime};

use aws_credential_types::{Credentials, provider::ProvideCredentials};
use aws_sigv4::{
    http_request::{SignableBody, SignableRequest, SignatureLocation, SigningSettings},
    sign::v4::SigningParams,
};
use base64::Engine;
use url::Url;

use crate::error::{ClusterAuthError, Result, context};

/// Token prefix required by the EKS authenticator webhook.
const TOKEN_PREFIX: &str = "k8s-aws-v1.";

/// Presigned URL expiration in seconds.
/// The AWS CLI uses 60s (`URL_TIMEOUT`), but since we write tokens to files and the
/// kube client reads them on each request, we use the maximum (15 minutes) to ensure
/// the presigned URL remains valid across the entire refresh interval.
const PRESIGNED_URL_EXPIRATION_SECS: u64 = 900;

/// How long we consider an IAM token valid for scheduling refresh.
/// Set to match `PRESIGNED_URL_EXPIRATION_SECS` (15 minutes).
pub const TOKEN_EXPIRATION_SECS: u64 = PRESIGNED_URL_EXPIRATION_SECS;

/// Refresh buffer: generate a new token when less than this many seconds remain.
/// We refresh at ~10 minutes (with 5 minutes of buffer before the 15-min expiry).
pub const REFRESH_BUFFER_SECS: u64 = 300;

/// Generate an EKS authentication token using IAM credentials.
///
/// This creates a presigned STS `GetCallerIdentity` URL with the `x-k8s-aws-id`
/// header set to the EKS cluster name, then encodes it as an EKS bearer token.
///
/// # Arguments
/// * `region` - AWS region of the EKS cluster (e.g., "us-east-1")
/// * `cluster_name` - EKS cluster name (used in the `x-k8s-aws-id` signed header)
///
/// # Returns
/// A bearer token string prefixed with `k8s-aws-v1.` that can be used to
/// authenticate to the EKS API server.
pub async fn generate_eks_token(region: &str, cluster_name: &str) -> Result<String> {
    tracing::debug!(
        region = %region,
        cluster_name = %cluster_name,
        "Generating EKS IAM token"
    );

    // Load AWS config from environment (supports IRSA, Pod Identity, instance profile, env vars)
    let config = aws_config::load_from_env().await;

    let credentials_provider = config.credentials_provider().ok_or_else(|| {
        ClusterAuthError::ConfigError(
            "No AWS credentials provider found in environment. \
             Ensure the workload has IAM credentials, e.g. via IRSA, Pod Identity or an ECS \
             task role."
                .to_owned()
                .into(),
        )
    })?;

    let credentials = credentials_provider
        .provide_credentials()
        .await
        // Not a `ConfigError`: the provider exists, and resolving through it can fail
        // transiently (e.g. the ECS container credentials endpoint), so a refresh retries it.
        .map_err(|e| ClusterAuthError::Internal(context("Failed to resolve AWS credentials", e)))?;

    sign_eks_token(credentials, region, cluster_name, SystemTime::now())
}

/// Sign an EKS authentication token with `credentials`, as of `time`.
///
/// Split from [`generate_eks_token`] so the signature can be checked against a fixed time.
fn sign_eks_token(
    credentials: Credentials,
    region: &str,
    cluster_name: &str,
    time: SystemTime,
) -> Result<String> {
    // Build the STS GetCallerIdentity URL
    // This matches the AWS CLI: GET https://sts.{region}.amazonaws.com/?Action=GetCallerIdentity&Version=2011-06-15
    let sts_url = format!("https://sts.{region}.amazonaws.com/");
    let mut url = Url::parse(&sts_url).map_err(|e| {
        ClusterAuthError::ConfigError(context(
            format!("Failed to parse STS URL for region {region}"),
            e,
        ))
    })?;
    url.query_pairs_mut()
        .append_pair("Action", "GetCallerIdentity")
        .append_pair("Version", "2011-06-15");

    // Configure signing — signature goes in query params, valid for 15 minutes
    let mut signing_settings = SigningSettings::default();
    signing_settings.signature_location = SignatureLocation::QueryParams;
    signing_settings.expires_in = Some(Duration::from_secs(PRESIGNED_URL_EXPIRATION_SECS));

    let identity = credentials.into();
    let signing_params = SigningParams::builder()
        .identity(&identity)
        .region(region)
        .name("sts")
        .time(time)
        .settings(signing_settings)
        .build()
        .map_err(|e| ClusterAuthError::ConfigError(context("Failed to build signing params", e)))?;

    // The x-k8s-aws-id header MUST be included as a signed header.
    // This cryptographically binds the token to a specific EKS cluster.
    // The EKS authenticator will include its own cluster name as this header
    // when validating — if it doesn't match what was signed, STS rejects it.
    let headers = [("x-k8s-aws-id", cluster_name)];

    let signable_request = SignableRequest::new(
        "GET",
        url.as_str(),
        headers.iter().map(|(k, v)| (*k, *v)),
        SignableBody::Bytes(&[]),
    )
    .map_err(|e| ClusterAuthError::ConfigError(context("Failed to create signable request", e)))?;

    let sign_output = aws_sigv4::http_request::sign(signable_request, &signing_params.into())
        .map_err(|e| ClusterAuthError::ConfigError(context("Failed to sign STS request", e)))?;

    let (sign_instructions, _) = sign_output.into_parts();

    // Apply signature parameters to URL (X-Amz-Algorithm, X-Amz-Credential,
    // X-Amz-Date, X-Amz-Expires, X-Amz-SignedHeaders, X-Amz-Signature,
    // and X-Amz-Security-Token if using session credentials).
    {
        let mut url_queries = url.query_pairs_mut();
        for (name, value) in sign_instructions.params() {
            url_queries.append_pair(name, value);
        }
    }

    // Encode as EKS token: "k8s-aws-v1." + base64url_no_pad(presigned_url)
    // The base64url encoding without padding matches the AWS CLI implementation.
    let token = format!(
        "{}{}",
        TOKEN_PREFIX,
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(url.as_str())
    );

    tracing::debug!(
        region = %region,
        cluster_name = %cluster_name,
        "EKS IAM token generated successfully"
    );

    Ok(token)
}

/// Calculate the expiry time for a freshly generated EKS IAM token.
///
/// Returns `now + TOKEN_EXPIRATION_SECS`.
pub fn token_expiry() -> SystemTime {
    SystemTime::now() + Duration::from_secs(TOKEN_EXPIRATION_SECS)
}

/// Check if an IAM token needs refresh based on its expiry time.
///
/// Returns `true` if the token expires within [`REFRESH_BUFFER_SECS`] (5 minutes).
pub fn needs_refresh(expiry: SystemTime) -> bool {
    match expiry.duration_since(SystemTime::now()) {
        Ok(remaining) => remaining < Duration::from_secs(REFRESH_BUFFER_SECS),
        Err(_) => true, // Already expired
    }
}

/// Calculate the duration until the next refresh is needed.
///
/// Returns `None` if the token should be refreshed immediately.
pub fn time_until_refresh(expiry: SystemTime) -> Option<Duration> {
    let remaining = expiry.duration_since(SystemTime::now()).ok()?;
    let refresh_in = remaining.saturating_sub(Duration::from_secs(REFRESH_BUFFER_SECS));

    // Don't return zero — caller should refresh immediately
    if refresh_in.is_zero() {
        None
    } else {
        Some(refresh_in)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    use super::*;

    /// 2026-01-02T03:04:05Z.
    const SIGNING_TIME: Duration = Duration::from_secs(1_767_323_045);

    fn presigned_url(token: &str) -> Url {
        let encoded = token.strip_prefix(TOKEN_PREFIX).unwrap();
        let url = URL_SAFE_NO_PAD.decode(encoded).unwrap();
        Url::parse(std::str::from_utf8(&url).unwrap()).unwrap()
    }

    /// The expected signature is what `aws eks get-token` (awscli 2.36) signs for the same
    /// credentials, cluster and time, with its URL timeout raised to
    /// [`PRESIGNED_URL_EXPIRATION_SECS`].
    #[test]
    fn token_matches_the_aws_cli() {
        let credentials = Credentials::new(
            "AKIDEXAMPLE",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            Some("session/token+with=reserved".to_owned()),
            None,
            "test",
        );

        let token = sign_eks_token(
            credentials,
            "us-east-1",
            "my-cluster",
            SystemTime::UNIX_EPOCH + SIGNING_TIME,
        )
        .unwrap();

        let url = presigned_url(&token);
        assert_eq!(url.host_str(), Some("sts.us-east-1.amazonaws.com"));
        let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["Action"], "GetCallerIdentity");
        assert_eq!(query["Version"], "2011-06-15");
        assert_eq!(
            query["X-Amz-Credential"],
            "AKIDEXAMPLE/20260102/us-east-1/sts/aws4_request"
        );
        assert_eq!(query["X-Amz-Date"], "20260102T030405Z");
        assert_eq!(query["X-Amz-Expires"], "900");
        assert_eq!(query["X-Amz-SignedHeaders"], "host;x-k8s-aws-id");
        assert_eq!(query["X-Amz-Security-Token"], "session/token+with=reserved");
        assert_eq!(
            query["X-Amz-Signature"],
            "cf9e1cfb488e56c0bbd982c8d675c2f728b91b09f6d8aaccc5a0dbfde46e96ec"
        );
        assert!(!token.contains('='));
    }

    #[test]
    fn token_is_bound_to_the_cluster_name() {
        let credentials = Credentials::new("AKIDEXAMPLE", "secret", None, None, "test");
        let time = SystemTime::UNIX_EPOCH + SIGNING_TIME;

        let token_a = sign_eks_token(credentials.clone(), "eu-west-1", "cluster-a", time).unwrap();
        let token_b = sign_eks_token(credentials, "eu-west-1", "cluster-b", time).unwrap();

        assert_ne!(token_a, token_b);
    }

    #[test]
    fn refresh_is_due_within_the_buffer_before_expiry() {
        let now = SystemTime::now();

        assert!(!needs_refresh(
            now + Duration::from_secs(TOKEN_EXPIRATION_SECS)
        ));
        assert!(needs_refresh(
            now + Duration::from_secs(REFRESH_BUFFER_SECS - 1)
        ));
        assert!(needs_refresh(now - Duration::from_secs(1)));
        assert_eq!(time_until_refresh(now), None);
    }
}
