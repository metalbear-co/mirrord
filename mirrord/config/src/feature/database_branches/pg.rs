use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::DatabaseBranchBaseConfig;

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
/// Example for AWS RDS:
/// ```json
/// {
///   "iam_auth": {
///     "type": "aws_rds",
///     "region": "us-east-1"
///   }
/// }
/// ```
///
/// Example with custom region environment variable:
/// ```json
/// {
///   "iam_auth": {
///     "type": "aws_rds",
///     "region_env": "MY_AWS_REGION"
///   }
/// }
/// ```
///
/// Example for GCP Cloud SQL:
/// ```json
/// {
///   "iam_auth": {
///     "type": "gcp_cloud_sql"
///   }
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PgIamAuthConfig {
    /// AWS RDS/Aurora IAM authentication.
    /// The init container must have AWS credentials (via IRSA, instance profile, or env vars).
    AwsRds {
        /// AWS region where the RDS instance is located.
        /// Takes precedence over `region_env` if both are specified.
        #[serde(skip_serializing_if = "Option::is_none")]
        region: Option<String>,

        /// Name of the environment variable containing the AWS region.
        /// If not specified, checks AWS_REGION then AWS_DEFAULT_REGION.
        #[serde(skip_serializing_if = "Option::is_none")]
        region_env: Option<String>,
    },
    /// GCP Cloud SQL IAM authentication.
    /// The init container must have GCP credentials (via Workload Identity or service account key).
    GcpCloudSql,
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
