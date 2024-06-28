use chrono::NaiveDate;
use http::HeaderName;
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

/// HTTP header containing CLI version.
/// Sent with each request to the mirrord operator.
pub static MIRRORD_CLI_VERSION_HEADER: HeaderName =
    HeaderName::from_static("x-mirrord-cli-version");

/// HTTP header containing client certificate.
/// Sent with each request to the mirrord operator (if available) except:
/// 1. Initial GET on the operator resource
/// 2. User certificate request
/// Required for making the target connection request.
pub static CLIENT_CERT_HEADER: HeaderName = HeaderName::from_static("x-client-der");

/// HTTP header containing client hostname.
/// Sent with each request to the mirrord operator (if available).
pub static CLIENT_HOSTNAME_HEADER: HeaderName = HeaderName::from_static("x-client-hostname");

/// HTTP header containing client name.
/// Sent with each request to the mirrord operator (if available).
pub static CLIENT_NAME_HEADER: HeaderName = HeaderName::from_static("x-client-name");

/// HTTP header containing operator session id.
/// Sent with target connection request.
pub static SESSION_ID_HEADER: HeaderName = HeaderName::from_static("x-session-id");
