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

/// Name of the HTTP header containing CLI version.
/// Sent with each request to the mirrord operator.
pub const MIRRORD_CLI_VERSION_HEADER_NAME: &str = "x-mirrord-cli-version";

/// Name of the HTTP header containing client certificate.
/// Send with each request to the mirrord operator except:
/// 1. Initial GET on the operator resource
/// 2. User certificate request
pub const CLIENT_CERT_HEADER_NAME: &str = "x-client-der";
