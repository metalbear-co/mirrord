//! The `validate_config` tool: checks the content of a `mirrord.json` or `mirrord-up.yaml`
//! against the config schema compiled into this binary, so an agent gets the answer for the
//! mirrord version the user actually runs, without any network access.
//!
//! Whether a config is valid is decided the way mirrord itself decides it: by deserializing it into
//! the config types, and for a `mirrord.json` also by generating and verifying the final config,
//! which is what `mirrord exec` and `mirrord verify-config` do. The schema generated from the
//! config types is not authoritative: it doesn't know about serde aliases or about hand-written
//! deserializers that accept more (or less) than it describes. It is used only to explain a config
//! that failed to deserialize, because it reports every problem at once, each with its location
//! and, where the schema lists them, the allowed values, where serde stops at the first error.

use std::{ops::Not, path::Path, sync::LazyLock};

use jsonschema::{ValidationError, Validator, error::ValidationErrorKind, paths::Location};
use mirrord_config::{
    LayerFileConfig,
    config::{ConfigContext, ConfigError, MirrordConfig},
    env_key::{EnvKey, MIRRORD_ENV_KEY},
};
use mirrord_up::UpConfig;
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use thiserror::Error;

/// Which config file the content belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
pub enum ConfigFormat {
    /// The configuration of a single mirrord session (`mirrord exec -f mirrord.json`).
    #[serde(rename = "mirrord.json")]
    MirrordJson,
    /// The multi-service configuration read by `mirrord up`.
    #[serde(rename = "mirrord-up.yaml")]
    MirrordUpYaml,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ValidateConfigArgs {
    /// Which kind of config file `content` is.
    pub format: ConfigFormat,
    /// The complete file content, exactly as it would be written to disk.
    pub content: String,
    /// The session key the config will be used with, which is what `{{ key }}` renders to: the
    /// `--key` passed to `mirrord up` (by default the username) or `MIRRORD_KEY`. Only matters
    /// for configs whose templates depend on the key's value; a placeholder is used when omitted.
    #[serde(default)]
    pub key: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ValidateConfigOutput {
    /// `true` when `issues` is empty.
    pub valid: bool,
    pub issues: Vec<ConfigIssue>,
}

/// One problem found in the config.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ConfigIssue {
    /// JSON pointer to the offending value, e.g. `/feature/network/incoming/mode`. Empty when the
    /// problem is with the whole file, such as a syntax error.
    pub path: String,
    pub message: String,
    /// The values accepted at `path`, when the schema enumerates them. For an unknown field,
    /// these are the field names allowed next to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_values: Option<Vec<Value>>,
}

/// Failures of the tool itself, as opposed to problems found in the config.
#[derive(Debug, Error)]
pub enum ValidateConfigError {
    #[error("the config schema compiled into mirrord is invalid: {0}")]
    InvalidSchema(#[source] &'static ValidationError<'static>),
}

/// A compiled schema, with the raw schema kept around to look up the allowed field names.
struct Schema {
    raw: Value,
    validator: Result<Validator, ValidationError<'static>>,
}

impl Schema {
    fn new<T: JsonSchema>() -> Self {
        let raw = schema_for!(T).to_value();
        let validator = jsonschema::validator_for(&raw);
        Self { raw, validator }
    }
}

static LAYER_SCHEMA: LazyLock<Schema> = LazyLock::new(Schema::new::<LayerFileConfig>);
static UP_SCHEMA: LazyLock<Schema> = LazyLock::new(Schema::new::<UpConfig>);

/// Placeholder for `{{ key }}` in a `mirrord-up.yaml` when no key is given. Any value works for
/// templates that only interpolate the key.
const TEMPLATE_KEY: &str = "mirrord-mcp-validate";

pub fn validate_config(
    ValidateConfigArgs {
        format,
        content,
        key,
    }: ValidateConfigArgs,
) -> Result<ValidateConfigOutput, ValidateConfigError> {
    let issues = match format {
        ConfigFormat::MirrordJson => {
            // Isolated from this process's environment, which is the MCP client's rather than the
            // one the config will be used in. Without a key, `render` takes it from the config's
            // own `key` field or generates one, as `mirrord exec` does.
            let mut context = ConfigContext::default()
                .strict_env(true)
                .override_env_opt(MIRRORD_ENV_KEY, key);
            let rendered =
                LayerFileConfig::render(&content, Path::new("mirrord.json"), &mut context)
                    .map_err(|error| error.to_string());
            let parsed = rendered.and_then(|rendered| {
                serde_json::from_str(&rendered).map_err(|error| error.to_string())
            });
            match parsed {
                Ok(value) => match check::<LayerFileConfig>(&value, &LAYER_SCHEMA)? {
                    Ok(config) => verify(config, &mut context).err().into_iter().collect(),
                    Err(issues) => issues,
                },
                Err(message) => vec![file_issue(message)],
            }
        }
        ConfigFormat::MirrordUpYaml => {
            let key = EnvKey::Provided(key.unwrap_or_else(|| TEMPLATE_KEY.to_owned()));
            let rendered =
                mirrord_up::render_template(&content, &key).map_err(|error| error.to_string());
            let parsed = rendered.and_then(|rendered| {
                serde_saphyr::from_str(&rendered).map_err(|error| error.to_string())
            });
            match parsed {
                Ok(value) => {
                    let mut issues = check::<UpConfig>(&value, &UP_SCHEMA)?
                        .err()
                        .unwrap_or_default();
                    issues.extend(check_config_patches(&value)?);
                    issues
                }
                Err(message) => vec![file_issue(message)],
            }
        }
    };

    Ok(ValidateConfigOutput {
        valid: issues.is_empty(),
        issues,
    })
}

/// Generates the final config and verifies it, like `mirrord exec` and `mirrord verify-config`.
///
/// An empty target is not treated as final: it may still be given on the command line (`-t`) or
/// picked in an IDE.
fn verify(config: LayerFileConfig, context: &mut ConfigContext) -> Result<(), ConfigIssue> {
    let mut context = std::mem::take(context).empty_target_final(false);
    config
        .generate_config(&mut context)
        .and_then(|config| config.verify(&mut context))
        .map_err(|error: ConfigError| ConfigIssue {
            path: String::new(),
            message: error.to_string(),
            allowed_values: None,
        })
}

/// An issue with the file as a whole: a template or syntax error that prevented reading it.
fn file_issue(message: String) -> ConfigIssue {
    ConfigIssue {
        path: String::new(),
        message,
        allowed_values: None,
    }
}

/// Validates the `config_patch` of every service in a `mirrord-up.yaml`.
///
/// `UpConfig` types the patch as arbitrary JSON, so its own schema accepts anything there, while
/// `mirrord up` merges it into the mirrord config it generates for the service and fails on a
/// result that isn't a valid mirrord config. The generated config depends on resolving targets in
/// the cluster, so the merge can't be reproduced here; instead the patch is checked on its own as
/// a mirrord config. Nearly every mirrord config field is optional, so any valid patch is also a
/// valid config fragment, and this catches invalid values and misspelled options.
fn check_config_patches(up_config: &Value) -> Result<Vec<ConfigIssue>, ValidateConfigError> {
    let Some(services) = up_config.get("services").and_then(Value::as_object) else {
        return Ok(Vec::new());
    };

    let mut issues = Vec::new();
    for (service, config) in services {
        let Some(patch) = config.get("config_patch") else {
            continue;
        };
        let prefix = format!("/services/{}/config_patch", escape_pointer_token(service));
        issues.extend(
            check::<LayerFileConfig>(patch, &LAYER_SCHEMA)?
                .err()
                .into_iter()
                .flatten()
                .map(|issue| ConfigIssue {
                    path: format!("{prefix}{}", issue.path),
                    ..issue
                }),
        );
    }

    Ok(issues)
}

/// Deserializes `value` into `T`. When that fails, the issues come from `schema` if it rejects the
/// value too, since it explains every problem with allowed values, and from the serde error
/// otherwise.
fn check<T: DeserializeOwned>(
    value: &Value,
    schema: &'static Schema,
) -> Result<Result<T, Vec<ConfigIssue>>, ValidateConfigError> {
    let serde_error = match serde_path_to_error::deserialize::<_, T>(value) {
        Ok(config) => return Ok(Ok(config)),
        Err(error) => error,
    };

    let validator = schema
        .validator
        .as_ref()
        .map_err(ValidateConfigError::InvalidSchema)?;
    let issues: Vec<_> = validator
        .iter_errors(value)
        .flat_map(|error| schema_issues(&error, &schema.raw))
        .collect();
    if issues.is_empty().not() {
        return Ok(Err(issues));
    }

    Ok(Err(vec![ConfigIssue {
        path: pointer_from_serde_path(serde_error.path()),
        message: serde_error.into_inner().to_string(),
        allowed_values: None,
    }]))
}

/// Turns one schema violation into issues. An unknown-fields violation becomes one issue per
/// unknown field, pointing at the field itself.
fn schema_issues(error: &ValidationError<'_>, raw_schema: &Value) -> Vec<ConfigIssue> {
    let path = error.instance_path().as_str().to_owned();

    match error.kind() {
        ValidationErrorKind::AnyOf { context } | ValidationErrorKind::OneOfNotValid { context }
            if let Some(branch) = intended_branch(context, error) =>
        {
            branch
                .iter()
                .flat_map(|error| schema_issues(error, raw_schema))
                .collect()
        }
        ValidationErrorKind::AdditionalProperties { unexpected } => {
            let allowed = allowed_properties(error, raw_schema);
            unexpected
                .iter()
                .map(|field| ConfigIssue {
                    path: format!("{path}/{}", escape_pointer_token(field)),
                    message: format!("unknown field `{field}`"),
                    allowed_values: allowed.clone(),
                })
                .collect()
        }
        _ => {
            let allowed_values = allowed_values(error);
            let message = match (&allowed_values, error.kind()) {
                // The generic "not valid under any of the schemas" says nothing useful when the
                // alternatives are just the allowed values.
                (
                    Some(_),
                    ValidationErrorKind::AnyOf { .. } | ValidationErrorKind::OneOfNotValid { .. },
                ) => format!("{} is not an allowed value", error.instance()),
                _ => error.to_string(),
            };
            vec![ConfigIssue {
                path,
                message,
                allowed_values,
            }]
        }
    }
}

/// The one alternative of a failed `anyOf`/`oneOf` that the value was evidently meant to match,
/// judged by it being the only alternative of the right JSON type.
///
/// Every optional field is rendered by schemars as `anyOf: [<field schema>, {type: null}]`, and
/// config fields often accept a short form (a string) or a full object. Without this, a typo deep
/// inside any of them would only be reported as "not valid under any of the schemas" at the
/// top-most optional field.
fn intended_branch<'e>(
    branches: &'e [Vec<ValidationError<'static>>],
    error: &ValidationError<'_>,
) -> Option<&'e [ValidationError<'static>]> {
    let mut plausible = branches
        .iter()
        .filter(|branch| is_wrong_type(branch, error.instance_path()).not());

    match (plausible.next(), plausible.next()) {
        (Some(branch), None) => Some(branch),
        _ => None,
    }
}

/// Whether the errors of an alternative show the value at `path` has the wrong JSON type for it,
/// directly or because every alternative of a nested `anyOf`/`oneOf` does.
fn is_wrong_type(branch: &[ValidationError<'_>], path: &Location) -> bool {
    branch.iter().any(|error| {
        error.instance_path() == path
            && match error.kind() {
                ValidationErrorKind::Type { .. } => true,
                ValidationErrorKind::AnyOf { context }
                | ValidationErrorKind::OneOfNotValid { context } => {
                    context.iter().all(|nested| is_wrong_type(nested, path))
                }
                _ => false,
            }
    })
}

/// The values enumerated by the schema that `error` was checked against: an `enum`, a `const`, or
/// an `anyOf`/`oneOf` of those (which is how schemars renders documented enum variants and
/// optional enums).
fn allowed_values(error: &ValidationError<'_>) -> Option<Vec<Value>> {
    let values = match error.kind() {
        ValidationErrorKind::Enum { options } => options.as_array()?.clone(),
        ValidationErrorKind::Constant { expected_value } => vec![expected_value.clone()],
        ValidationErrorKind::AnyOf { context } | ValidationErrorKind::OneOfNotValid { context } => {
            context
                .iter()
                .flatten()
                .filter(|branch| branch.instance_path() == error.instance_path())
                .filter_map(allowed_values)
                .flatten()
                .collect()
        }
        _ => return None,
    };

    values.is_empty().not().then_some(values)
}

/// The field names declared next to an `additionalProperties: false`, found by following the
/// error's schema path back to the object schema that holds it.
fn allowed_properties(error: &ValidationError<'_>, raw_schema: &Value) -> Option<Vec<Value>> {
    let keyword_location = error.schema_path().as_str();
    let (object_location, _) = keyword_location.rsplit_once('/')?;
    let properties = raw_schema
        .pointer(object_location)?
        .get("properties")?
        .as_object()?;

    Some(properties.keys().cloned().map(Value::String).collect())
}

/// RFC 6901 escaping for one JSON pointer segment.
fn escape_pointer_token(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

fn pointer_from_serde_path(path: &serde_path_to_error::Path) -> String {
    path.iter()
        .filter_map(|segment| match segment {
            serde_path_to_error::Segment::Seq { index } => Some(index.to_string()),
            serde_path_to_error::Segment::Map { key } => Some(escape_pointer_token(key)),
            serde_path_to_error::Segment::Enum { .. } | serde_path_to_error::Segment::Unknown => {
                None
            }
        })
        .map(|token| format!("/{token}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn validate(format: ConfigFormat, content: &str) -> ValidateConfigOutput {
        validate_config(ValidateConfigArgs {
            format,
            content: content.to_owned(),
            key: None,
        })
        .unwrap()
    }

    fn single_issue(format: ConfigFormat, content: &str) -> ConfigIssue {
        let mut output = validate(format, content);
        assert!(output.valid.not());
        assert_eq!(output.issues.len(), 1, "{:?}", output.issues);
        output.issues.remove(0)
    }

    #[test]
    fn schemas_compile() {
        assert!(LAYER_SCHEMA.validator.is_ok());
        assert!(UP_SCHEMA.validator.is_ok());
    }

    #[test]
    fn valid_mirrord_json() {
        let output = validate(
            ConfigFormat::MirrordJson,
            r#"{
                "target": { "path": "deployment/app", "namespace": "default" },
                "feature": {
                    "network": { "incoming": { "mode": "steal" } },
                    "fs": "read"
                }
            }"#,
        );
        assert!(output.valid, "{:?}", output.issues);
        assert!(output.issues.is_empty());
    }

    #[test]
    fn templated_mirrord_json() {
        let output = validate(
            ConfigFormat::MirrordJson,
            r#"{ "key": "{{ git_branch | default(value='main') }}", "feature": { "network": { "incoming": { "mode": "steal", "http_filter": { "header_filter": "x-session: {{ key }}" } } } } }"#,
        );
        assert!(output.valid, "{:?}", output.issues);
    }

    #[test]
    fn unknown_field() {
        let issue = single_issue(
            ConfigFormat::MirrordJson,
            r#"{ "feature": { "network": { "incomin": {} } } }"#,
        );
        assert_eq!(issue.path, "/feature/network/incomin");
        let allowed = issue.allowed_values.unwrap();
        assert!(allowed.contains(&json!("incoming")), "{allowed:?}");
        assert!(allowed.contains(&json!("outgoing")), "{allowed:?}");
    }

    #[test]
    fn bad_enum_value() {
        let issue = single_issue(
            ConfigFormat::MirrordJson,
            r#"{ "feature": { "network": { "incoming": { "mode": "foo" } } } }"#,
        );
        assert_eq!(issue.path, "/feature/network/incoming/mode");
        let allowed = issue.allowed_values.unwrap();
        assert!(allowed.contains(&json!("steal")), "{allowed:?}");
        assert!(allowed.contains(&json!("mirror")), "{allowed:?}");
    }

    #[test]
    fn json_syntax_error() {
        let issue = single_issue(ConfigFormat::MirrordJson, r#"{ "feature": "#);
        assert_eq!(issue.path, "");
        assert!(issue.message.contains("line"), "{}", issue.message);
    }

    #[test]
    fn valid_up_yaml() {
        let output = validate(
            ConfigFormat::MirrordUpYaml,
            r#"
services:
  consumer:
    target:
      path: deployment/consumer
    run:
      command: ["python", "-m", "http.server", "{{ key }}"]
"#,
        );
        assert!(output.valid, "{:?}", output.issues);
    }

    #[test]
    fn up_yaml_unknown_field() {
        let issue = single_issue(
            ConfigFormat::MirrordUpYaml,
            r#"
services:
  consumer:
    run:
      command: ["python"]
    bogus: true
"#,
        );
        assert_eq!(issue.path, "/services/consumer/bogus");
        let allowed = issue.allowed_values.unwrap();
        assert!(allowed.contains(&json!("run")), "{allowed:?}");
    }

    #[test]
    fn up_yaml_valid_config_patch() {
        let output = validate(
            ConfigFormat::MirrordUpYaml,
            r#"
services:
  worker:
    config_patch:
      feature:
        split_queues:
          "*":
            queue_type: SQS
            jq_filter: '.Body | fromjson | .headers["x-origin"] == "{{ key }}"'
    run:
      command: ["echo"]
"#,
        );
        assert!(output.valid, "{:?}", output.issues);
    }

    #[test]
    fn up_yaml_invalid_config_patch() {
        let issue = single_issue(
            ConfigFormat::MirrordUpYaml,
            r#"
services:
  worker:
    config_patch:
      feature:
        split_queues: NOT_A_SPLIT_QUEUE_CONFIG
    run:
      command: ["echo"]
"#,
        );
        assert_eq!(
            issue.path,
            "/services/worker/config_patch/feature/split_queues"
        );
    }

    #[test]
    fn up_yaml_config_patch_unknown_field() {
        let issue = single_issue(
            ConfigFormat::MirrordUpYaml,
            r#"
services:
  worker:
    config_patch:
      feature:
        netwrk: {}
    run:
      command: ["echo"]
"#,
        );
        assert_eq!(issue.path, "/services/worker/config_patch/feature/netwrk");
        assert!(issue.allowed_values.unwrap().contains(&json!("network")));
    }

    #[test]
    fn up_yaml_missing_services() {
        let issue = single_issue(ConfigFormat::MirrordUpYaml, "common: {}\n");
        assert_eq!(issue.path, "");
        assert!(issue.message.contains("services"), "{}", issue.message);
    }
}
