use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{ConnectionSourceKind, DatabaseBranchBaseConfig};

/// When configuring a branch for PostgreSQL, set `type` to `pg`.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
pub struct PgBranchConfig {
    #[serde(flatten)]
    pub base: DatabaseBranchBaseConfig,

    #[serde(default)]
    pub copy: PgBranchCopyConfig,

    /// IAM authentication for the source database.
    /// Use this when your source database (AWS RDS, GCP Cloud SQL) requires IAM authentication
    /// instead of password-based authentication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iam_auth: Option<PgIamAuthConfig>,
}

/// IAM authentication configuration for cloud-managed PostgreSQL databases.
///
/// Environment variable sources follow the same pattern as `connection.url`:
/// - `{ "type": "env", "variable": "VAR_NAME" }` - direct env var from pod spec
/// - `{ "type": "env_from", "variable": "VAR_NAME" }` - from configMapRef/secretRef
///
/// Example for AWS RDS with defaults (uses AWS_REGION, AWS_ACCESS_KEY_ID, etc.):
/// ```json
/// {
///   "iam_auth": {
///     "type": "aws_rds"
///   }
/// }
/// ```
///
/// Example with custom environment variables:
/// ```json
/// {
///   "iam_auth": {
///     "type": "aws_rds",
///     "region": { "type": "env", "variable": "MY_AWS_REGION" },
///     "access_key_id": { "type": "env_from", "variable": "AWS_KEY" }
///   }
/// }
/// ```
///
/// Example for GCP Cloud SQL with credentials from a secret:
/// ```json
/// {
///   "iam_auth": {
///     "type": "gcp_cloud_sql",
///     "credentials_json": { "type": "env_from", "variable": "GOOGLE_APPLICATION_CREDENTIALS_JSON" }
///   }
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PgIamAuthConfig {
    /// AWS RDS/Aurora IAM authentication.
    /// The init container must have AWS credentials (via IRSA, instance profile, or env vars).
    AwsRds {
        /// AWS region. If not specified, uses AWS_REGION or AWS_DEFAULT_REGION.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        region: Option<ConnectionSourceKind>,

        /// AWS Access Key ID. If not specified, uses AWS_ACCESS_KEY_ID.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        access_key_id: Option<ConnectionSourceKind>,

        /// AWS Secret Access Key. If not specified, uses AWS_SECRET_ACCESS_KEY.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        secret_access_key: Option<ConnectionSourceKind>,

        /// AWS Session Token (for temporary credentials). If not specified, uses
        /// AWS_SESSION_TOKEN.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_token: Option<ConnectionSourceKind>,
    },
    /// GCP Cloud SQL IAM authentication.
    /// The init container must have GCP credentials (via Workload Identity or service account key).
    /// Use either `credentials_json` OR `credentials_path`, not both.
    GcpCloudSql {
        /// Inline service account JSON key content.
        /// Specify the env var that contains the raw JSON content of the service account key.
        /// Example: `{"type": "env", "variable": "GOOGLE_APPLICATION_CREDENTIALS_JSON"}`
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credentials_json: Option<ConnectionSourceKind>,

        /// Path to service account JSON key file.
        /// Specify the env var that contains the file path to the service account key.
        /// The file must be accessible from the init container.
        /// Example: `{"type": "env", "variable": "GOOGLE_APPLICATION_CREDENTIALS"}`
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credentials_path: Option<ConnectionSourceKind>,

        /// GCP project ID. If not specified, uses GOOGLE_CLOUD_PROJECT or GCP_PROJECT.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<ConnectionSourceKind>,
    },
}

/// Users can choose from the following copy mode to bootstrap their PostgreSQL branch database:
///
/// - Empty
///
///   Creates an empty database. If the source DB connection options are found from the chosen
///   target, mirrord operator extracts the database name and create an empty DB. Otherwise, mirrord
///   operator looks for the `name` field from the branch DB config object. This option is useful
///   for users that run DB migrations themselves before starting the application.
///
/// - Schema
///
///   Creates an empty database and copies schema of all tables.
///
/// - All
///
///   Copies both schema and data of all tables. This option shall only be used
///   when the data volume of the source database is minimal.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum PgBranchCopyConfig {
    Empty {
        tables: Option<HashMap<String, PgBranchTableCopyConfig>>,
    },

    Schema {
        tables: Option<HashMap<String, PgBranchTableCopyConfig>>,
    },

    All,
}

impl Default for PgBranchCopyConfig {
    fn default() -> Self {
        PgBranchCopyConfig::Empty {
            tables: Default::default(),
        }
    }
}

/// In addition to copying an empty database or all tables' schema, mirrord operator
/// will copy data from the source DB when an array of table configs are specified.
///
/// Example:
///
/// ```json
/// {
///   "users": {
///     "filter": "name = 'alice' OR name = 'bob'"
///   },
///   "orders": {
///     "filter": "created_at > '2025-01-01'"
///   }
/// }
/// ```
///
/// With the config above, only alice and bob from the `users` table and orders
/// created after the given timestamp will be copied.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
pub struct PgBranchTableCopyConfig {
    pub filter: Option<String>,
}
