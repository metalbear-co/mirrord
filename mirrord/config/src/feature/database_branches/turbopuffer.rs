use std::ops::Not;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{BranchBaseConfig, ConnectionParamsConfig};
use crate::{config::ConfigError, feature::database_branches::ParamSource};

/// The param naming the env var that holds the source namespace, rewritten to the branch.
///
/// Located in [`ConnectionParamsVars::extra`](super::ConnectionParamsVars::extra).
pub const NAMESPACE_PARAM: &str = "namespace";

/// The param the operator reads the turbopuffer API key from.
pub const API_KEY_PARAM: &str = "api_key";

/// The param naming the turbopuffer region the namespace lives in.
pub const REGION_PARAM: &str = "region";

/// The param naming the full API endpoint, for dedicated clusters.
pub const BASE_URL_PARAM: &str = "base_url";

/// When configuring a branch for a turbopuffer namespace, set `type` to `turbopuffer`.
///
/// The branch namespace is a copy-on-write clone made by turbopuffer itself, so - like an S3
/// branch - it has no pod in the cluster and takes no `image` nor `version`. It is otherwise
/// a normal branch: the standard `id`, `ttl_secs`/`ttl_mins` and `creation_timeout_secs`
/// options all apply.
///
/// turbopuffer clients pick the namespace per request, so the app has to read the namespace
/// name from an env var for mirrord to redirect it. The `namespace` param names that var;
/// once the branch exists, the operator points it at the branch namespace. The `api_key`
/// param names where the operator reads the API key it branches with, and `region` (or
/// `base_url`) where the namespace lives. Those two are only read, never rewritten.
///
/// Example:
/// ```json
/// {
///   "type": "turbopuffer",
///   "source": {
///     "params": {
///       "namespace": "TPUF_NAMESPACE",
///       "api_key": "TURBOPUFFER_API_KEY",
///       "region": { "env_var_name": "TURBOPUFFER_REGION", "value": "gcp-us-central1" }
///     }
///   },
///   "copy": { "mode": "all" }
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurbopufferBranchConfig {
    #[serde(flatten)]
    pub base: BranchBaseConfig,

    /// #### feature.db_branches[].source (type: turbopuffer) {#feature-db_branches-turbopuffer-source}
    ///
    /// Where to read the source namespace, the API key and the region from, in the same
    /// params shape the other engines use for their connection details - `type` picks how
    /// the variables are resolved on the target (`env` for the pod spec's own `env`,
    /// `env_from` for its `envFrom` sources), and is auto-detected when omitted.
    ///
    /// The params a turbopuffer branch takes:
    ///
    /// - `namespace` (required): the env var holding the source namespace name. The operator
    ///   rewrites it to the branch namespace, so it must name an env var.
    /// - `api_key` (required): the API key used to branch and later delete the namespace.
    /// - `region`: the turbopuffer region, e.g. `gcp-us-central1`.
    /// - `base_url`: the full API endpoint, for dedicated clusters. Exactly one of `region` and
    ///   `base_url` must be set.
    ///
    /// ```json
    /// {
    ///   "source": {
    ///     "params": {
    ///       "namespace": "TPUF_NAMESPACE",
    ///       "api_key": { "secret": "turbopuffer", "key": "api-key" },
    ///       "base_url": "TURBOPUFFER_BASE_URL"
    ///     }
    ///   }
    /// }
    /// ```
    ///
    /// Each value is as flexible as any other engine's params: a Kubernetes Secret
    /// (`{ "secret": "my-secret", "key": "namespace" }`), a literal
    /// (`{ "env_var_name": "TURBOPUFFER_REGION", "value": "gcp-us-central1" }`), or a regex
    /// extracting the name out of a larger variable
    /// (`{ "env_var_name": "TPUF_URI", "value_pattern": "..." }`).
    #[serde(alias = "connection")]
    pub source: ConnectionParamsConfig,

    /// #### feature.db_branches[].copy (type: turbopuffer) {#feature-db_branches-turbopuffer-copy}
    ///
    /// How the branch namespace is seeded from the source namespace.
    #[serde(default)]
    pub copy: TurbopufferBranchCopyConfig,
}

impl TurbopufferBranchConfig {
    pub fn verify(&self) -> Result<(), ConfigError> {
        self.base.verify()?;
        if let Some(profile) = &self.base.profile {
            return Err(ConfigError::Conflict(format!(
                "`feature.db_branches[].profile` is not supported for turbopuffer branches \
                (requested `{profile}`); a turbopuffer branch has no pod to configure"
            )));
        }
        let params = &self.source.params;

        let namespace = params
            .extra
            .get(NAMESPACE_PARAM)
            .filter(|values| values.is_empty().not())
            .ok_or_else(|| {
                ConfigError::Conflict(format!(
                    "`feature.db_branches[].source.params.{NAMESPACE_PARAM}` \
                    is required for turbopuffer branches"
                ))
            })?;
        if namespace.0.len() > 1 {
            return Err(ConfigError::Conflict(format!(
                "`feature.db_branches[].source.params.{NAMESPACE_PARAM}` takes a single source \
                for turbopuffer branches; a branch clones one namespace, so listing several \
                would point them all at the same clone"
            )));
        }
        if !namespace.0.iter().all(ParamSource::names_env_var) {
            return Err(ConfigError::Conflict(format!(
                "for turbopuffer branches, \
                `feature.db_branches[].source.params.{NAMESPACE_PARAM}` must specify \
                the environment variable to fill with the name of the branch namespace"
            )));
        }

        let is_set = |param: &str| {
            params
                .extra
                .get(param)
                .is_some_and(|values| values.is_empty().not())
        };
        for param in [NAMESPACE_PARAM, API_KEY_PARAM, REGION_PARAM, BASE_URL_PARAM] {
            let blank = params
                .extra
                .get(param)
                .into_iter()
                .flat_map(|values| values.0.iter())
                .any(param_source_is_blank);
            if blank {
                return Err(ConfigError::Conflict(format!(
                    "`feature.db_branches[].source.params.{param}` is blank"
                )));
            }
        }
        if is_set(API_KEY_PARAM).not() {
            return Err(ConfigError::Conflict(format!(
                "`feature.db_branches[].source.params.{API_KEY_PARAM}` \
                is required for turbopuffer branches"
            )));
        }

        // A literal endpoint is the one param that can point the operator at a host of the
        // config author's choosing, and the operator sends the resolved API key there. It has
        // to come off the target, so that naming it needs the same access as running there.
        let literal_base_url = params
            .extra
            .get(BASE_URL_PARAM)
            .into_iter()
            .flat_map(|values| values.0.iter())
            .any(|source| {
                matches!(
                    source,
                    ParamSource::Env {
                        value: Some(..),
                        ..
                    }
                )
            });
        if literal_base_url {
            return Err(ConfigError::Conflict(format!(
                "`feature.db_branches[].source.params.{BASE_URL_PARAM}` cannot be a literal \
                value for turbopuffer branches; name the env var or the Secret the target \
                reads its endpoint from"
            )));
        }

        if is_set(REGION_PARAM) == is_set(BASE_URL_PARAM) {
            return Err(ConfigError::Conflict(format!(
                "exactly one of `feature.db_branches[].source.params.{REGION_PARAM}` and \
                `feature.db_branches[].source.params.{BASE_URL_PARAM}` must be set for \
                turbopuffer branches"
            )));
        }

        let accepted = [NAMESPACE_PARAM, API_KEY_PARAM, REGION_PARAM, BASE_URL_PARAM];
        let unknown = [
            ("url", params.url.as_ref()),
            ("host", params.host.as_ref()),
            ("port", params.port.as_ref()),
            ("user", params.user.as_ref()),
            ("password", params.password.as_ref()),
            ("database", params.database.as_ref()),
        ]
        .into_iter()
        .chain(
            params
                .extra
                .iter()
                .filter(|(key, ..)| accepted.contains(&key.as_str()).not())
                .map(|(key, values)| (key.as_str(), Some(values))),
        )
        .find(|(.., values)| {
            values
                .as_ref()
                .is_some_and(|values| values.is_empty().not())
        });
        if let Some((unknown, ..)) = unknown {
            return Err(ConfigError::Conflict(format!(
                "`{unknown}` is not a valid `feature.db_branches[].source.params` entry for a \
                 turbopuffer branch; the accepted params are {}.",
                accepted.map(|param| format!("`{param}`")).join(", "),
            )));
        }

        Ok(())
    }

    /// The sources of the one param the operator rewrites on the local app.
    pub fn namespace_sources(&self) -> impl Iterator<Item = &ParamSource> {
        self.source
            .params
            .extra
            .get(NAMESPACE_PARAM)
            .into_iter()
            .flatten()
    }
}

/// Whether a param source carries nothing the operator can resolve a value from.
fn param_source_is_blank(source: &ParamSource) -> bool {
    let blank = |value: &str| value.trim().is_empty();

    match source {
        ParamSource::Variable(name) => blank(name),
        ParamSource::Secret { name, key, .. } => blank(name) || blank(key),
        ParamSource::Pattern {
            env_var_name,
            value_pattern,
        } => blank(env_var_name) || blank(value_pattern),
        ParamSource::Env { env_var_name, .. } => blank(env_var_name),
        ParamSource::GcpSecretManager { secret_ref, .. }
        | ParamSource::AwsSecretsManager { secret_ref, .. } => blank(secret_ref),
        ParamSource::ConfigMap { key, .. } => key.as_deref().is_some_and(blank),
    }
}

/// Users can choose from the following copy modes to bootstrap their turbopuffer branch:
///
/// - Empty (default)
///
///   Reserves a fresh namespace name with nothing in it; turbopuffer creates the namespace on
///   the app's first write.
///
/// - All
///
///   Branches the source namespace: an instant copy-on-write clone with every document and
///   the schema. Writes to either side never reach the other.
///
/// ```json
/// { "copy": { "mode": "all" } }
/// ```
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize, Default)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
pub enum TurbopufferBranchCopyConfig {
    #[default]
    Empty,
    All,
}
