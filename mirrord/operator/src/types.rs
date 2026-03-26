use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LicenseInfoOwned {
    pub name: String,
    pub organization: String,
    pub expire_at: NaiveDate,
    /// Fingerprint of the operator license.
    pub fingerprint: Option<String>,
    /// Subscription id encoded in the operator license extension.
    pub subscription_id: Option<String>,
}

/// Name of HTTP header containing CLI version.
/// Sent with each request to the mirrord operator.
pub const MIRRORD_CLI_VERSION_HEADER: &str = "x-mirrord-cli-version";

/// Name of HTTP header containing client certificate.
/// Sent with each request to the mirrord operator (if available) except:
/// 1. Initial GET on the operator resource
/// 2. User certificate request
///
/// Required for making the target connection request.
pub const CLIENT_CERT_HEADER: &str = "x-client-der";

/// Name of HTTP header containing client hostname.
/// Sent with each request to the mirrord operator (if available).
pub const CLIENT_HOSTNAME_HEADER: &str = "x-client-hostname";

/// Name of HTTP header containing client name.
/// Sent with each request to the mirrord operator (if available).
pub const CLIENT_NAME_HEADER: &str = "x-client-name";

/// Name of HTTP header containing operator session id.
/// Sent with target connection request.
pub const SESSION_ID_HEADER: &str = "x-session-id";

/// Code returned in error responses from the operator, when reconnecting to a session is no longer
/// possible.
///
/// HTTP 410 Gone.
pub const RECONNECT_NOT_POSSIBLE_CODE: u16 = 410;

/// Reason returned in error responses from the operator, when reconnecting to a session is no
/// longer possible.
pub const RECONNECT_NOT_POSSIBLE_REASON: &str = "ReconnectNotPossible";

/// Kubernetes label key identifying resources owned by the mirrord operator.
pub const OPERATOR_OWNERSHIP_LABEL: &str = "operator.metalbear.co/owner";

/// Name of the environment variable that overrides the default operator isolation marker.
pub const OPERATOR_ISOLATION_MARKER_ENV: &str = "OPERATOR_ISOLATION_MARKER";

/// Default value for the [`OPERATOR_OWNERSHIP_LABEL`] when
/// [`OPERATOR_ISOLATION_MARKER_ENV`] is not set.
pub const DEFAULT_OPERATOR_ISOLATION_MARKER: &str = "mirrord-operator";

/// Label that prevents the sync controller from copying a CR to the default cluster.
///
/// When a user forces a single-cluster session (`multi_cluster: false`), the CLI sets
/// this label on the created CR (e.g. `BranchDatabase`). The sync controller checks for
/// it and skips the CR, while the local controller (with a matching label selector) picks
/// it up and handles it on the primary cluster instead.
pub const MULTI_CLUSTER_SKIP_SYNC_LABEL: &str = "operator.metalbear.co/multi-cluster-skip-sync";
