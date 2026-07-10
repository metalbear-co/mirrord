use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{ConnectionSource, DatabaseBranchBaseConfig, ParamSource};
use crate::config::{ConfigContext, ConfigError};

/// Prefix of the env vars the operator injects into the branch container, one per declared
/// connection param (e.g. the `password` param becomes `MIRRORD_PARAM_PASSWORD`).
pub const MIRRORD_PARAM_PREFIX: &str = "MIRRORD_PARAM_";

/// Built-in env var holding the branch id, always available to `command`/`args`/`env`.
pub const BUILTIN_BRANCH_ID_VAR: &str = "MIRRORD_BRANCH_ID";

/// Built-in env var holding the shared `name` field, available when `name` is set.
pub const BUILTIN_DATABASE_NAME_VAR: &str = "MIRRORD_DATABASE_NAME";

/// When branching a database, cache, or any other stateful service that mirrord has no built-in
/// engine for, set `type` to `generic`.
///
/// A generic branch runs the container image you supply, starting empty - there are no copy
/// modes, IAM auth, or schema handling. The operator resolves each parameter you declare under
/// `connection.params` from the target pod and injects it into the branch container as an env
/// var named `MIRRORD_PARAM_<NAME>` (a Secret-backed param arrives as a `secretKeyRef`; the
/// operator never reads its value). Reference these in `command`, `args`, and `env` with
/// Kubernetes' native `$(VAR)` syntax so the branch bootstraps itself with the *same* values the
/// app already uses. mirrord then only redirects the app's `host`/`port` vars to the branch -
/// everything else in the app's environment keeps working unchanged.
///
/// Two built-ins are always available alongside the params: `MIRRORD_BRANCH_ID` and, when the
/// shared `name` field is set, `MIRRORD_DATABASE_NAME`.
///
/// #### feature.db_branches[].image (type: generic) {#feature-db_branches-generic-image}
///
/// Full image reference for the branch container, including the tag
/// (e.g. `docker.io/library/influxdb:2.7`). Required. The shared `version` field is not
/// allowed for generic branches - the tag lives here.
///
/// #### feature.db_branches[].port (type: generic) {#feature-db_branches-generic-port}
///
/// The port the branched service listens on. Required. Used as the default readiness probe
/// target and as the port the app's connection is redirected to.
///
/// #### feature.db_branches[].command / args (type: generic) {#feature-db_branches-generic-command}
///
/// Optional entrypoint override for the branch container. Values may reference declared params
/// with `$(MIRRORD_PARAM_<NAME>)`; use `$$(...)` for a literal `$(...)`. Values written
/// directly into `args` are visible in the pod spec - reference params via `$(VAR)` instead of
/// inlining secrets.
///
/// #### feature.db_branches[].env (type: generic) {#feature-db_branches-generic-env}
///
/// Extra environment variables for the branch container. Values support the same
/// `$(MIRRORD_PARAM_<NAME>)` references as `command`/`args`. Keys must not start with
/// `MIRRORD_PARAM_` or collide with the built-ins.
///
/// #### feature.db_branches[].readiness (type: generic) {#feature-db_branches-generic-readiness}
///
/// Readiness check for the branch container. Defaults to a TCP probe on `port`.
///
/// ```json
/// { "readiness": { "type": "http_get", "path": "/health" } }
/// ```
/// ```json
/// { "readiness": { "type": "exec", "command": ["redis-cli", "ping"] } }
/// ```
///
/// Example - an empty InfluxDB branch, bootstrapped with the app's own token/org/bucket so the
/// app's untouched credential vars stay valid against the branch:
///
/// ```json
/// {
///   "type": "generic",
///   "id": "my-influx-branch",
///   "image": "docker.io/library/influxdb:2.7",
///   "port": 8086,
///   "connection": {
///     "params": {
///       "host": { "env_var_name": "INFLUXDB_URL", "value_pattern": "https?://([^:/]+)" },
///       "port": { "env_var_name": "INFLUXDB_URL", "value_pattern": ":([0-9]+)" },
///       "token": { "secret": "influx-creds", "key": "token" },
///       "org": "INFLUXDB_ORG",
///       "bucket": "INFLUXDB_BUCKET"
///     }
///   },
///   "env": {
///     "DOCKER_INFLUXDB_INIT_MODE": "setup",
///     "DOCKER_INFLUXDB_INIT_USERNAME": "admin",
///     "DOCKER_INFLUXDB_INIT_PASSWORD": "mirrord-branch",
///     "DOCKER_INFLUXDB_INIT_ADMIN_TOKEN": "$(MIRRORD_PARAM_TOKEN)",
///     "DOCKER_INFLUXDB_INIT_ORG": "$(MIRRORD_PARAM_ORG)",
///     "DOCKER_INFLUXDB_INIT_BUCKET": "$(MIRRORD_PARAM_BUCKET)"
///   }
/// }
/// ```
///
/// `token`, `org`, and `bucket` are custom params - any key works under `connection.params`,
/// not just the fixed `host`/`port`/`user`/`password`/`database` slots. The connection must use
/// params mode (URL-shaped env vars are covered by `value_pattern` on `host`/`port`), and
/// `gcp_secret_manager` sources are not supported for generic branches.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenericBranchConfig {
    #[serde(flatten)]
    pub base: DatabaseBranchBaseConfig,

    /// Full image reference for the branch container, including the tag.
    pub image: String,

    /// The port the branched service listens on.
    pub port: u16,

    /// Entrypoint command override for the branch container.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,

    /// Entrypoint args override for the branch container.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,

    /// Extra environment variables for the branch container.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,

    /// Readiness check for the branch container. Defaults to a TCP probe on `port`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<GenericReadinessConfig>,
}

/// <!--${internal}-->
/// Readiness check for a generic branch container. Documented on [`GenericBranchConfig`].
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum GenericReadinessConfig {
    /// TCP probe on the branch `port` (the default when `readiness` is not set).
    Tcp,
    /// Run a command inside the branch container; ready when it exits 0.
    Exec { command: Vec<String> },
    /// HTTP GET probe; ready on a 2xx/3xx response. `port` defaults to the branch `port`.
    HttpGet {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        port: Option<u16>,
    },
}

/// The fixed connection param slots, in the order they are injected into the branch container.
pub const FIXED_PARAM_SLOTS: [&str; 5] = ["host", "port", "user", "password", "database"];

impl GenericBranchConfig {
    /// Names of all declared connection params (fixed slots that are set, then extras),
    /// as written in the config.
    pub fn declared_params(&self) -> Vec<&str> {
        let params = match &self.base.connection {
            ConnectionSource::Params(config) => &config.params,
            _ => return Vec::new(),
        };

        let fixed = [
            ("host", &params.host),
            ("port", &params.port),
            ("user", &params.user),
            ("password", &params.password),
            ("database", &params.database),
        ];

        fixed
            .into_iter()
            .filter(|(_, sources)| sources.is_some())
            .map(|(name, _)| name)
            .chain(params.extra.keys().map(String::as_str))
            .collect()
    }

    /// The env var name a declared param is injected under in the branch container.
    pub fn param_env_var_name(param: &str) -> String {
        format!("{MIRRORD_PARAM_PREFIX}{}", param.to_uppercase())
    }

    /// Env var names of the `host` and `port` sources - the only vars the operator redirects
    /// on the local process for a generic branch.
    pub(crate) fn collect_redirected_env_keys<'a>(&'a self, out: &mut Vec<&'a str>) {
        let ConnectionSource::Params(config) = &self.base.connection else {
            return;
        };

        [&config.params.host, &config.params.port]
            .into_iter()
            .filter_map(|sources| sources.as_ref())
            .flatten()
            .for_each(|source| source.collect_env_keys(out));
    }

    pub fn verify(&self, context: &mut ConfigContext) -> Result<(), ConfigError> {
        self.base.verify()?;

        if self.base.version.is_some() {
            return Err(ConfigError::Conflict(
                "`feature.db_branches[].version` is not allowed for generic branches; \
                 the image tag is part of `feature.db_branches[].image`."
                    .to_owned(),
            ));
        }

        let params = match &self.base.connection {
            ConnectionSource::Params(config) => &config.params,
            ConnectionSource::Url { .. } | ConnectionSource::FlatUrl { .. } => {
                return Err(ConfigError::Conflict(
                    "generic branches require `feature.db_branches[].connection` in params \
                     mode. For a URL-shaped env var, extract `host` and `port` with \
                     `value_pattern` params instead."
                        .to_owned(),
                ));
            }
        };

        // GSM sources are unsupported for generic branches (no mirrord init binary in the
        // branch pod to fetch them).
        let all_sources = [
            &params.host,
            &params.port,
            &params.user,
            &params.password,
            &params.database,
        ]
        .into_iter()
        .filter_map(|sources| sources.as_ref())
        .chain(params.extra.values());
        for source in all_sources.flatten() {
            if matches!(source, ParamSource::GcpSecretManager { .. }) {
                return Err(ConfigError::Conflict(
                    "`gcp_secret_manager` connection params are not supported for generic \
                     branches."
                        .to_owned(),
                ));
            }
        }

        // Custom keys become env var names, so they must be valid identifiers and must not
        // collide (case-insensitively) with the fixed slots or each other.
        let mut seen_upper: BTreeMap<String, &str> = FIXED_PARAM_SLOTS
            .iter()
            .map(|slot| (slot.to_uppercase(), *slot))
            .collect();
        for key in params.extra.keys() {
            if !is_valid_param_key(key) {
                return Err(ConfigError::InvalidValue {
                    name: "feature.db_branches[].connection.params".into(),
                    provided: key.clone(),
                    error: "custom connection param keys must match [A-Za-z_][A-Za-z0-9_]* \
                            (they become env var names)"
                        .into(),
                });
            }

            if let Some(existing) = seen_upper.insert(key.to_uppercase(), key) {
                return Err(ConfigError::InvalidValue {
                    name: "feature.db_branches[].connection.params".into(),
                    provided: key.clone(),
                    error: format!(
                        "collides with the `{existing}` connection param: both would be \
                         injected as `{}`",
                        Self::param_env_var_name(key),
                    )
                    .into(),
                });
            }
        }

        // User env keys must not shadow the injected vars.
        for key in self.env.keys() {
            if key.starts_with(MIRRORD_PARAM_PREFIX)
                || key == BUILTIN_BRANCH_ID_VAR
                || key == BUILTIN_DATABASE_NAME_VAR
            {
                return Err(ConfigError::InvalidValue {
                    name: "feature.db_branches[].env".into(),
                    provided: key.clone(),
                    error: format!(
                        "`{MIRRORD_PARAM_PREFIX}*`, `{BUILTIN_BRANCH_ID_VAR}` and \
                         `{BUILTIN_DATABASE_NAME_VAR}` are injected by mirrord and cannot be \
                         set in `env`",
                    )
                    .into(),
                });
            }
        }

        self.verify_var_references(context)?;

        if params.host.is_none() && params.port.is_none() {
            context.add_warning(
                "A generic db branch declares neither `host` nor `port` under \
                 `connection.params`: the branch will be created, but nothing will be \
                 redirected to it."
                    .to_owned(),
            );
        }

        Ok(())
    }

    /// Validates every `$(VAR)` reference in `command`/`args`/`env` values against the
    /// declared params, built-ins, and user env keys; warns about declared params (other than
    /// `host`/`port`, which exist for redirection) that are never referenced.
    fn verify_var_references(&self, context: &mut ConfigContext) -> Result<(), ConfigError> {
        let declared = self.declared_params();

        let mut valid: BTreeSet<String> = declared
            .iter()
            .map(|param| Self::param_env_var_name(param))
            .collect();
        valid.insert(BUILTIN_BRANCH_ID_VAR.to_owned());
        if self.base.name.is_some() {
            valid.insert(BUILTIN_DATABASE_NAME_VAR.to_owned());
        }
        valid.extend(self.env.keys().cloned());

        let values = self
            .command
            .iter()
            .flatten()
            .chain(self.args.iter().flatten())
            .chain(self.env.values());

        let mut referenced: BTreeSet<String> = BTreeSet::new();
        for value in values {
            for var in scan_var_references(value) {
                if !valid.contains(&var) {
                    if var == BUILTIN_DATABASE_NAME_VAR {
                        return Err(ConfigError::Conflict(format!(
                            "`$({BUILTIN_DATABASE_NAME_VAR})` is referenced, but \
                             `feature.db_branches[].name` is not set.",
                        )));
                    }

                    return Err(ConfigError::InvalidValue {
                        name: "feature.db_branches[].command/args/env".into(),
                        provided: format!("$({var})"),
                        error: format!(
                            "unknown variable reference; available: {}. Use `$$(...)` for a \
                             literal `$(...)`",
                            valid.iter().cloned().collect::<Vec<_>>().join(", "),
                        )
                        .into(),
                    });
                }
                referenced.insert(var);
            }

            // Shell-wrapper bootstraps (`command: ["sh", "-c", "... $MIRRORD_PARAM_X ..."]`)
            // reference the injected vars with shell syntax instead of kubelet `$(...)`.
            // They only suppress the unreferenced-param warning below - unknown shell vars
            // are NOT validated, since a script may legitimately use any variable.
            for var in scan_shell_references(value) {
                if valid.contains(&var) {
                    referenced.insert(var);
                }
            }
        }

        for param in declared {
            // `host`/`port` exist so the operator can redirect the app; they are not
            // expected to be referenced by the branch container.
            if param == "host" || param == "port" {
                continue;
            }
            let var = Self::param_env_var_name(param);
            if !referenced.contains(&var) {
                context.add_warning(format!(
                    "A generic db branch declares the `{param}` connection param, but \
                     `$({var})` is never referenced in `command`, `args`, or `env`.",
                ));
            }
        }

        Ok(())
    }
}

/// Extracts shell-style references (`$VAR` / `${VAR}`) from a value. Used only to suppress
/// the unreferenced-param warning for shell-wrapper bootstraps; never for error validation.
fn scan_shell_references(value: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let bytes = value.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        // `$(...)` (kubelet) and `$$` (escape) are handled by `scan_var_references`.
        if matches!(bytes.get(i + 1), Some(b'(') | Some(b'$')) {
            i += 2;
            continue;
        }

        let mut j = i + 1;
        let braced = bytes.get(j) == Some(&b'{');
        if braced {
            j += 1;
        }
        let start = j;
        while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
            j += 1;
        }
        if j > start && (!braced || bytes.get(j) == Some(&b'}')) {
            refs.push(value[start..j].to_owned());
        }
        i = j + 1;
    }

    refs
}

/// Custom connection param keys become env var names: `[A-Za-z_][A-Za-z0-9_]*`.
fn is_valid_param_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Collects the variable names a generic branch's `command`/`args`/`env` value refers to.
///
/// In those fields the user writes Kubernetes' native `$(VAR)` syntax, where `VAR` is the name of
/// an env var present in the branch container at startup. Those names are:
/// - `MIRRORD_PARAM_<NAME>` - one per declared `connection.params` entry (see
///   [`MIRRORD_PARAM_PREFIX`]).
/// - [`BUILTIN_BRANCH_ID_VAR`] and [`BUILTIN_DATABASE_NAME_VAR`] - the always-available built-ins.
/// - any key the user defined in the branch's own `env` map.
///
/// The kubelet does the actual substitution when the pod starts; this function only *extracts* the
/// referenced names so both the CLI (`GenericBranchConfig::verify_var_references`) and the
/// operator (which re-validates because CRDs can be created by non-CLI clients) can check that
/// every referenced name is one the branch will actually provide.
///
/// `$$(...)` is the Kubernetes escape for a literal `$(...)`, so it yields no reference. A `$` not
/// followed by `(`, and an unterminated `$(...`, are plain text and yield nothing.
///
/// ```ignore
/// // Injected as MIRRORD_PARAM_TOKEN; MIRRORD_BRANCH_ID is a built-in.
/// assert_eq!(
///     scan_var_references("--token $(MIRRORD_PARAM_TOKEN) --id $(MIRRORD_BRANCH_ID)"),
///     vec!["MIRRORD_PARAM_TOKEN".to_owned(), "MIRRORD_BRANCH_ID".to_owned()],
/// );
/// // `$$(...)` is escaped, so nothing is referenced.
/// assert!(scan_var_references("literal $$(NOT_A_REF)").is_empty());
/// ```
pub fn scan_var_references(value: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let mut rest = value;

    while let Some(dollar) = rest.find('$') {
        rest = &rest[dollar + 1..];

        // `$$` escapes the following `$`, so `$$(VAR)` is a literal `$(VAR)`.
        if let Some(after_escape) = rest.strip_prefix('$') {
            rest = after_escape;
            continue;
        }

        // `$(VAR)` is a reference; anything else after `$` is literal text.
        if let Some(inner) = rest.strip_prefix('(')
            && let Some(end) = inner.find(')')
        {
            refs.push(inner[..end].to_owned());
            rest = &inner[end + 1..];
        }
    }

    refs
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)] // Tests build JSON fixtures with `json[key]`; a panic just fails the test.
mod tests {
    use super::*;

    #[test]
    fn scan_references_basic() {
        assert_eq!(
            scan_var_references("--token $(MIRRORD_PARAM_TOKEN) --x $(FOO)"),
            vec!["MIRRORD_PARAM_TOKEN".to_owned(), "FOO".to_owned()],
        );
    }

    #[test]
    fn scan_references_escaped() {
        assert_eq!(
            scan_var_references("literal $$(NOT_A_REF) then $(REAL)"),
            vec!["REAL".to_owned()],
        );
        // Triple dollar: `$$` escape followed by a real `$(...)`.
        assert_eq!(scan_var_references("$$$(REAL)"), vec!["REAL".to_owned()],);
    }

    #[test]
    fn scan_references_unterminated() {
        assert!(scan_var_references("$(UNTERMINATED").is_empty());
        assert!(scan_var_references("plain $ sign").is_empty());
    }

    #[test]
    fn scan_shell_references_basic() {
        assert_eq!(
            scan_shell_references("cluster-init --password \"$MIRRORD_PARAM_PASSWORD\" x"),
            vec!["MIRRORD_PARAM_PASSWORD".to_owned()],
        );
        assert_eq!(
            scan_shell_references("echo ${MIRRORD_PARAM_TOKEN} done"),
            vec!["MIRRORD_PARAM_TOKEN".to_owned()],
        );
        // Kubelet refs and escapes are not shell refs.
        assert!(scan_shell_references("$(MIRRORD_PARAM_X) $$(Y)").is_empty());
    }

    /// A shell-wrapper bootstrap (`sh -c "... $MIRRORD_PARAM_PASSWORD ..."`) must not
    /// trigger the unreferenced-param warning.
    #[test]
    fn shell_reference_suppresses_unreferenced_warning() {
        let json = serde_json::json!({
            "type": "generic",
            "image": "couchbase:community-7.6.2",
            "port": 8091,
            "connection": {
                "params": {
                    "host": "COUCHBASE_HOST",
                    "password": "COUCHBASE_PASSWORD"
                }
            },
            "command": ["sh", "-c", "init --password \"$MIRRORD_PARAM_PASSWORD\"; wait"]
        });
        let mut context = ConfigContext::default();
        parse(json)
            .verify(&mut context)
            .expect("config should verify");
        assert!(!context.has_warnings(), "{:?}", context.into_warnings());
    }

    #[test]
    fn param_key_validity() {
        assert!(is_valid_param_key("vhost"));
        assert!(is_valid_param_key("_private"));
        assert!(is_valid_param_key("cache_name2"));
        assert!(!is_valid_param_key("2fast"));
        assert!(!is_valid_param_key("with-dash"));
        assert!(!is_valid_param_key(""));
    }

    use crate::feature::database_branches::DatabaseBranchConfig;

    fn parse(json: serde_json::Value) -> GenericBranchConfig {
        match serde_json::from_value(json).expect("config should deserialize") {
            DatabaseBranchConfig::Generic(config) => *config,
            other => panic!("expected a generic branch config, got {other:?}"),
        }
    }

    fn influx_config() -> serde_json::Value {
        serde_json::json!({
            "type": "generic",
            "id": "my-influx-branch",
            "image": "docker.io/library/influxdb:2.7",
            "port": 8086,
            "connection": {
                "params": {
                    "host": { "env_var_name": "INFLUXDB_URL", "value_pattern": "https?://([^:/]+)" },
                    "port": { "env_var_name": "INFLUXDB_URL", "value_pattern": ":([0-9]+)" },
                    "token": { "secret": "influx-creds", "key": "token" },
                    "org": "INFLUXDB_ORG",
                    "bucket": "INFLUXDB_BUCKET"
                }
            },
            "env": {
                "DOCKER_INFLUXDB_INIT_MODE": "setup",
                "DOCKER_INFLUXDB_INIT_ADMIN_TOKEN": "$(MIRRORD_PARAM_TOKEN)",
                "DOCKER_INFLUXDB_INIT_ORG": "$(MIRRORD_PARAM_ORG)",
                "DOCKER_INFLUXDB_INIT_BUCKET": "$(MIRRORD_PARAM_BUCKET)"
            }
        })
    }

    #[test]
    fn deserialize_and_verify_influx_example() {
        let config = parse(influx_config());
        assert_eq!(config.image, "docker.io/library/influxdb:2.7");
        assert_eq!(config.port, 8086);
        assert_eq!(
            config.declared_params(),
            vec!["host", "port", "bucket", "org", "token"],
        );

        let mut context = ConfigContext::default();
        config.verify(&mut context).expect("config should verify");
        assert!(!context.has_warnings(), "{:?}", context.into_warnings());
    }

    #[test]
    fn version_is_rejected() {
        let mut json = influx_config();
        json["version"] = "2.7".into();
        let mut context = ConfigContext::default();
        parse(json).verify(&mut context).unwrap_err();
    }

    #[test]
    fn url_connection_is_rejected() {
        let mut json = influx_config();
        json["connection"] = serde_json::json!({ "url": { "type": "env", "variable": "URL" } });
        let mut context = ConfigContext::default();
        parse(json).verify(&mut context).unwrap_err();
    }

    #[test]
    fn gsm_param_is_rejected() {
        let mut json = influx_config();
        json["connection"]["params"]["token"] = serde_json::json!({
            "gcp_secret_manager": "projects/p/secrets/s/versions/latest"
        });
        let mut context = ConfigContext::default();
        parse(json).verify(&mut context).unwrap_err();
    }

    #[test]
    fn invalid_extra_key_is_rejected() {
        let mut json = influx_config();
        json["connection"]["params"]["with-dash"] = "SOME_VAR".into();
        let mut context = ConfigContext::default();
        parse(json).verify(&mut context).unwrap_err();
    }

    #[test]
    fn extra_key_shadowing_fixed_slot_is_rejected() {
        let mut json = influx_config();
        // Uppercases to `MIRRORD_PARAM_PORT`, colliding with the fixed `port` slot.
        json["connection"]["params"]["PORT"] = "SOME_VAR".into();
        let mut context = ConfigContext::default();
        parse(json).verify(&mut context).unwrap_err();
    }

    #[test]
    fn undeclared_reference_is_rejected() {
        let mut json = influx_config();
        json["env"]["EXTRA"] = "$(MIRRORD_PARAM_MISSING)".into();
        let mut context = ConfigContext::default();
        parse(json).verify(&mut context).unwrap_err();
    }

    #[test]
    fn escaped_reference_is_ignored() {
        let mut json = influx_config();
        json["env"]["EXTRA"] = "$$(MIRRORD_PARAM_MISSING)".into();
        let mut context = ConfigContext::default();
        parse(json)
            .verify(&mut context)
            .expect("escaped reference should not be validated");
    }

    #[test]
    fn database_name_builtin_requires_name() {
        let mut json = influx_config();
        json["env"]["DB"] = "$(MIRRORD_DATABASE_NAME)".into();
        let mut context = ConfigContext::default();
        parse(json.clone()).verify(&mut context).unwrap_err();

        json["name"] = "my-db".into();
        let mut context = ConfigContext::default();
        parse(json)
            .verify(&mut context)
            .expect("name is set, builtin should be valid");
    }

    #[test]
    fn reserved_env_key_is_rejected() {
        let mut json = influx_config();
        json["env"]["MIRRORD_PARAM_FOO"] = "x".into();
        let mut context = ConfigContext::default();
        parse(json).verify(&mut context).unwrap_err();
    }

    #[test]
    fn missing_host_and_port_warns() {
        let mut json = influx_config();
        let params = json["connection"]["params"].as_object_mut().unwrap();
        params.remove("host");
        params.remove("port");
        let mut context = ConfigContext::default();
        parse(json)
            .verify(&mut context)
            .expect("config should verify");
        assert!(
            context
                .into_warnings()
                .iter()
                .any(|warning| warning.contains("nothing will be redirected"))
        );
    }

    #[test]
    fn unreferenced_param_warns() {
        let mut json = influx_config();
        json["env"]
            .as_object_mut()
            .unwrap()
            .remove("DOCKER_INFLUXDB_INIT_ORG");
        let mut context = ConfigContext::default();
        parse(json)
            .verify(&mut context)
            .expect("config should verify");
        assert!(
            context
                .into_warnings()
                .iter()
                .any(|warning| warning.contains("MIRRORD_PARAM_ORG"))
        );
    }

    #[test]
    fn redirected_env_keys_are_host_and_port_sources_only() {
        let config = parse(influx_config());
        let mut keys = Vec::new();
        config.collect_redirected_env_keys(&mut keys);
        // Both host and port come from the same composite var.
        assert_eq!(keys, vec!["INFLUXDB_URL", "INFLUXDB_URL"]);
    }
}
