use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::DatabaseBranchBaseConfig;

/// Default name of the env var the operator injects to redirect the app to the branch emulator.
pub fn default_spanner_emulator_host_var() -> String {
    "SPANNER_EMULATOR_HOST".to_owned()
}

/// When configuring a branch for Google Cloud Spanner, set `type` to `spanner`.
///
/// The branch runs the Cloud Spanner emulator. mirrord redirects the app to it by injecting
/// `SPANNER_EMULATOR_HOST`, so the app's own `project`/`instance`/`database` values keep working
/// and now resolve against the emulator. The branch init sidecar recreates the matching instance
/// and database in the emulator and, for `schema`/`all`, copies from the real Spanner using the
/// target pod's Google service account (Workload Identity / Application Default Credentials).
///
/// Spanner's source identifiers live flat under `connection.params`: `project`, `instance`, and
/// `database_id` each name the env var (on the target pod) that holds the value. They are read
/// only - the app never has them overridden - so they belong with the connection source rather
/// than at the top level. `database_id` (not `database`) keeps the locator from colliding with the
/// fixed `database` connection slot. `emulator_host` is the exception: it is the var mirrord *sets*
/// on the local process to point at the branch, so it stays top-level.
///
/// Example:
/// ```json
/// {
///   "type": "spanner",
///   "connection": {
///     "params": {
///       "project": "GOOGLE_CLOUD_PROJECT",
///       "instance": "SPANNER_INSTANCE_ID",
///       "database_id": "SPANNER_DATABASE_ID"
///     }
///   },
///   "copy": { "mode": "schema" }
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpannerBranchConfig {
    #[serde(flatten)]
    pub base: DatabaseBranchBaseConfig,

    #[serde(default)]
    pub copy: SpannerBranchCopyConfig,

    /// #### feature.db_branches[].emulator_host (type: spanner) {#feature-db_branches-spanner-emulator_host}
    ///
    /// The *name* of the env var mirrord sets on your process to redirect it to the branch
    /// emulator - not an address. mirrord fills that var's *value* with the branch emulator's
    /// `host:port` at runtime; you only choose which variable it writes to.
    ///
    /// Defaults to `SPANNER_EMULATOR_HOST`, the variable every official Spanner client library
    /// checks on its own, so the default redirects the app with no code change. Leave it unset
    /// unless your app ignores that built-in detection and reads the emulator endpoint from a
    /// differently-named variable (for example it reads `MY_EMULATOR` and passes that endpoint to
    /// the client explicitly); then set this to that variable's name so mirrord writes the address
    /// where your app actually looks.
    #[serde(default = "default_spanner_emulator_host_var")]
    pub emulator_host: String,
}

/// Users can choose from the following copy mode to bootstrap their Spanner branch database:
///
/// - Empty
///
///   Creates an empty database. The instance and database matching the app's configuration are
///   created in the emulator, but no schema or data is copied. Useful when the app runs its own
///   migrations.
///
/// - Schema
///
///   Creates the database and copies the DDL (tables and indexes) of the source database.
///
/// - All
///
///   Copies both schema and data. Use only when the source data volume is minimal.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
pub enum SpannerBranchCopyConfig {
    Empty {
        tables: Option<BTreeMap<String, SpannerBranchTableCopyConfig>>,
    },

    Schema {
        tables: Option<BTreeMap<String, SpannerBranchTableCopyConfig>>,
    },

    All,
}

impl Default for SpannerBranchCopyConfig {
    fn default() -> Self {
        SpannerBranchCopyConfig::Empty {
            tables: Default::default(),
        }
    }
}

pub type SpannerBranchTableCopyConfig = super::BranchItemCopyConfig;
