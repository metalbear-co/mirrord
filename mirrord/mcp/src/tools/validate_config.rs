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

use std::{fmt, ops::Not, path::Path, str::FromStr};

use jsonschema::{
    ValidationError,
    error::{TypeKind, ValidationErrorKind},
    paths::Location,
    types::JsonType,
};
use mirrord_config::{
    LayerFileConfig,
    config::{ConfigContext, ConfigError, MirrordConfig},
    env_key::{EnvKey, MIRRORD_ENV_KEY},
    target::{FAIL_PARSE_DEPLOYMENT_OR_POD, TARGET_PATH_FORMATS, Target},
};
use mirrord_up::{LAYER_CONFIG_PATHS, ServiceMode, UpConfig, UpError};
use schemars::JsonSchema;
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{DeserializeOwned, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;
use serde_saphyr::{DuplicateKeyPolicy, Spanned};
use thiserror::Error;

use crate::schema::{LAYER_SCHEMA, Node, Schema, UP_SCHEMA, expand};

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
                let value = serde_json::from_str(&rendered).map_err(|error| error.to_string())?;
                Ok((rendered, value))
            });
            match parsed {
                Ok((rendered, value)) => {
                    let mut issues = check_layer_config(&value, context)?;
                    // Read the way `mirrord exec` reads it, which rejects a field given twice
                    // where the `Value` above silently keeps the last one.
                    if issues.is_empty()
                        && let Err(error) = serde_path_to_error::deserialize::<_, LayerFileConfig>(
                            &mut serde_json::Deserializer::from_str(&rendered),
                        )
                    {
                        let mut path = pointer_from_serde_path(error.path());
                        let message = error.into_inner().to_string();
                        // serde reports a repeated field at the object holding it.
                        if let Some((field, _)) = message
                            .strip_prefix("duplicate field `")
                            .and_then(|rest| rest.split_once('`'))
                        {
                            path = format!("{path}/{}", escape_pointer_token(field));
                        }
                        issues.push(ConfigIssue {
                            path,
                            message,
                            allowed_values: None,
                        });
                    }
                    issues
                }
                Err(message) => vec![file_issue(message)],
            }
        }
        ConfigFormat::MirrordUpYaml => {
            let key = EnvKey::Provided(key.unwrap_or_else(|| TEMPLATE_KEY.to_owned()));
            let rendered = mirrord_up::render_template(&content, &key)
                .map_err(|error| file_issue(error.to_string()));
            let parsed = rendered.and_then(|rendered| {
                serde_saphyr::from_str(&rendered).map_err(|error| ConfigIssue {
                    path: duplicate_key_pointer(&error, &rendered).unwrap_or_default(),
                    // The default rendering is meant for the program calling the parser, e.g. it
                    // suggests a `DuplicateKeyPolicy` for a key given twice.
                    message: error
                        .without_snippet()
                        .render_with_formatter(&serde_saphyr::UserMessageFormatter),
                    allowed_values: None,
                })
            });
            match parsed {
                Ok(value) => {
                    let mut issues = match check::<UpConfig>(&value, &UP_SCHEMA)? {
                        Ok(config) => config.verify().err().map(up_issue).into_iter().collect(),
                        Err(issues) => issues,
                    };
                    if issues.is_empty() {
                        issues.extend(check_service_settings(&value)?);
                    }
                    issues.extend(check_config_patches(&value)?);
                    issues
                }
                Err(issue) => vec![issue],
            }
        }
    };

    Ok(ValidateConfigOutput {
        valid: issues.is_empty(),
        issues,
    })
}

/// Validates a mirrord config: deserializes it, then generates the final config and verifies it,
/// like `mirrord exec` and `mirrord verify-config`.
///
/// An empty target is not treated as final: it may still be given on the command line (`-t`) or
/// picked in an IDE.
fn check_layer_config(
    value: &Value,
    context: ConfigContext,
) -> Result<Vec<ConfigIssue>, ValidateConfigError> {
    let config = match check::<LayerFileConfig>(value, &LAYER_SCHEMA)? {
        Ok(config) => config,
        Err(issues) => {
            return Ok(issues
                .into_iter()
                .map(|issue| target_path_issue(issue, value))
                .collect());
        }
    };

    let mut context = context.empty_target_final(false);
    Ok(config
        .generate_config(&mut context)
        .and_then(|config| config.verify(&mut context))
        .err()
        .map(|error: ConfigError| ConfigIssue {
            path: config_error_path(&error),
            message: error.to_string(),
            allowed_values: None,
        })
        .into_iter()
        .collect())
}

/// The JSON pointer of the setting a [`ConfigError`] names, where it names one as a dotted path
/// such as `startup_retry.max_ms` or `feature.preview.config_mounts[0].payload`. An index-less `[]`
/// (as in `feature.db_branches[].name`, meaning "any entry") points at the list itself.
fn config_error_path(error: &ConfigError) -> String {
    let name = match error {
        ConfigError::InvalidValue { name, .. } => name,
        ConfigError::ConflictAt { setting, .. } => setting,
        _ => return String::new(),
    };
    // Otherwise the name of an environment variable.
    if name.contains('.').not() {
        return String::new();
    }

    let name = name.trim_start_matches('.');
    let name = name.split_once("[]").map_or(name, |(list, _)| list);
    dotted_to_pointer(&name.replace('[', ".").replace(']', ""))
}

/// The JSON pointer of a dotted path such as `feature.network.incoming.http_filter`.
fn dotted_to_pointer(path: &str) -> String {
    path.split('.')
        .map(|segment| format!("/{}", escape_pointer_token(segment)))
        .collect()
}

/// Explains why the target path of a mirrord config doesn't parse, listing the forms it takes.
///
/// The schema takes any string as a target path, and `target` is an untagged enum, so serde
/// reports an invalid path only as matching none of the forms of `target`. Parsing the path on its
/// own, as mirrord does, gives the reason; the generic one is a guide for fixing a target at
/// runtime (e.g. checking it with `kubectl`), which the listed forms replace here.
fn target_path_issue(issue: ConfigIssue, config: &Value) -> ConfigIssue {
    if issue.path != "/target" {
        return issue;
    }

    let target = config.get("target");
    let (path, target_path) = match target.and_then(|target| target.get("path")) {
        Some(target_path) => ("/target/path", target_path),
        None => ("/target", target.unwrap_or(&Value::Null)),
    };
    let Some((target_path, Err(error))) = target_path
        .as_str()
        .map(|target_path| (target_path, Target::from_str(target_path)))
    else {
        return issue;
    };

    let message = match error {
        ConfigError::InvalidTarget(reason) if reason.contains(FAIL_PARSE_DEPLOYMENT_OR_POD) => {
            format!("`{target_path}` is not a valid target path")
        }
        ConfigError::InvalidTarget(reason) => {
            format!("`{target_path}` is not a valid target path: {reason}")
        }
        error => format!("`{target_path}` is not a valid target path: {error}"),
    };
    ConfigIssue {
        path: path.to_owned(),
        message,
        allowed_values: Some(
            TARGET_PATH_FORMATS
                .iter()
                .map(|format| Value::from(*format))
                .collect(),
        ),
    }
}

/// An issue found by [`UpConfig::verify`], pointing at the offending setting where the error names
/// it.
fn up_issue(error: UpError) -> ConfigIssue {
    let path = match &error {
        UpError::ContainerRunDirectory { service } => {
            format!("/services/{}/run/directory", escape_pointer_token(service))
        }
        _ => String::new(),
    };

    ConfigIssue {
        path,
        message: error.to_string(),
        allowed_values: None,
    }
}

/// The keys of a YAML document, each with the place it's written, to find the path of a key that
/// the parser reports only by its line and column.
enum YamlKeys {
    Mapping(Vec<(Spanned<Value>, YamlKeys)>),
    Sequence(Vec<YamlKeys>),
    Scalar,
}

impl<'de> Deserialize<'de> for YamlKeys {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct KeysVisitor;

        impl<'de> Visitor<'de> for KeysVisitor {
            type Value = YamlKeys;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("any YAML value")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<YamlKeys, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry()? {
                    entries.push(entry);
                }
                Ok(YamlKeys::Mapping(entries))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<YamlKeys, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element()? {
                    items.push(item);
                }
                Ok(YamlKeys::Sequence(items))
            }

            fn visit_bool<E>(self, _: bool) -> Result<YamlKeys, E> {
                Ok(YamlKeys::Scalar)
            }

            fn visit_i64<E>(self, _: i64) -> Result<YamlKeys, E> {
                Ok(YamlKeys::Scalar)
            }

            fn visit_u64<E>(self, _: u64) -> Result<YamlKeys, E> {
                Ok(YamlKeys::Scalar)
            }

            fn visit_f64<E>(self, _: f64) -> Result<YamlKeys, E> {
                Ok(YamlKeys::Scalar)
            }

            fn visit_str<E>(self, _: &str) -> Result<YamlKeys, E> {
                Ok(YamlKeys::Scalar)
            }

            fn visit_unit<E>(self) -> Result<YamlKeys, E> {
                Ok(YamlKeys::Scalar)
            }

            fn visit_none<E>(self) -> Result<YamlKeys, E> {
                Ok(YamlKeys::Scalar)
            }
        }

        deserializer.deserialize_any(KeysVisitor)
    }
}

impl YamlKeys {
    /// The JSON pointer of the key written at `location`.
    fn pointer_at(&self, location: &serde_saphyr::Location) -> Option<String> {
        match self {
            Self::Mapping(entries) => entries.iter().find_map(|(key, value)| {
                let segment = match &key.value {
                    Value::String(key) => escape_pointer_token(key),
                    key => escape_pointer_token(&key.to_string()),
                };
                if key.referenced.line() == location.line()
                    && key.referenced.column() == location.column()
                {
                    return Some(format!("/{segment}"));
                }
                value
                    .pointer_at(location)
                    .map(|rest| format!("/{segment}{rest}"))
            }),
            Self::Sequence(items) => items.iter().enumerate().find_map(|(index, item)| {
                item.pointer_at(location)
                    .map(|rest| format!("/{index}{rest}"))
            }),
            Self::Scalar => None,
        }
    }
}

/// The JSON pointer of the key a duplicate-key error is about, found by reading the document
/// again with duplicates allowed.
fn duplicate_key_pointer(error: &serde_saphyr::Error, content: &str) -> Option<String> {
    let serde_saphyr::Error::DuplicateMappingKey { location, .. } = error.without_snippet() else {
        return None;
    };
    let options = serde_saphyr::options! { duplicate_keys: DuplicateKeyPolicy::LastWins };
    serde_saphyr::from_str_with_options::<YamlKeys>(content, options)
        .ok()?
        .pointer_at(location)
}

/// An issue with the file as a whole: a template or syntax error that prevented reading it.
fn file_issue(message: String) -> ConfigIssue {
    ConfigIssue {
        path: String::new(),
        message,
        allowed_values: None,
    }
}

/// Runs the checks of a mirrord config on the settings of every service in a `mirrord-up.yaml` that
/// `mirrord up` copies into the mirrord config it generates for the service (per
/// [`LAYER_CONFIG_PATHS`]), such as the regexes of an `http_filter`.
///
/// `default_mode` is left out, as `mirrord up` translates its values, and so are the settings that
/// need the cluster to resolve, like `target`. So is the `http_filter` of a service in `replace`
/// mode, which `mirrord up` ignores (unless `mirrord up --mode` overrides the mode, which a file
/// can't tell).
fn check_service_settings(up_config: &Value) -> Result<Vec<ConfigIssue>, ValidateConfigError> {
    let Some(services) = up_config.get("services").and_then(Value::as_object) else {
        return Ok(Vec::new());
    };

    let mut issues = Vec::new();
    for (service, settings) in services {
        let service_pointer = format!("/services/{}", escape_pointer_token(service));
        let replace_mode = settings
            .get("default_mode")
            .and_then(|mode| ServiceMode::deserialize(mode).ok())
            == Some(ServiceMode::Replace);
        let mut layer_config = Value::Object(Default::default());
        let mut copied = Vec::new();
        for (up_path, layer_path) in LAYER_CONFIG_PATHS {
            let Some(setting) = up_path.strip_prefix("services.*.") else {
                continue;
            };
            if layer_path.starts_with("feature.").not()
                || setting == "default_mode"
                || (setting == "http_filter" && replace_mode)
            {
                continue;
            }
            let Some(value) = settings.get(setting) else {
                continue;
            };
            let layer_pointer = dotted_to_pointer(layer_path);
            insert_at(&mut layer_config, &layer_pointer, value.clone());
            copied.push((layer_pointer, format!("{service_pointer}/{setting}")));
        }
        if copied.is_empty() {
            continue;
        }

        let context = ConfigContext::default().strict_env(true);
        issues.extend(
            check_layer_config(&layer_config, context)?
                .into_iter()
                .map(|issue| {
                    let path = copied
                        .iter()
                        .find_map(|(layer_pointer, up_pointer)| {
                            let rest = issue.path.strip_prefix(layer_pointer.as_str())?;
                            Some(format!("{up_pointer}{rest}"))
                        })
                        .unwrap_or_else(|| service_pointer.clone());
                    ConfigIssue { path, ..issue }
                }),
        );
    }

    Ok(issues)
}

/// Sets `value` at `pointer` in `root`, creating the objects on the way.
fn insert_at(root: &mut Value, pointer: &str, value: Value) {
    let mut target = root;
    for segment in pointer.split('/').skip(1) {
        let Value::Object(fields) = target else {
            return;
        };
        target = fields
            .entry(segment.to_owned())
            .or_insert_with(|| Value::Object(Default::default()));
    }
    *target = value;
}

/// Validates the `config_patch` of every service in a `mirrord-up.yaml`.
///
/// `UpConfig` types the patch as arbitrary JSON, so its own schema accepts anything there, while
/// `mirrord up` merges it into the mirrord config it generates for the service and fails on a
/// result that isn't a valid mirrord config. The generated config depends on resolving targets in
/// the cluster, so the merge can't be reproduced here; instead the patch is checked on its own as
/// a mirrord config, including the semantic checks `mirrord up` runs on the merged result (such as
/// the jq filters of split queues). Nearly every mirrord config field is optional, so any valid
/// patch is also a valid config fragment.
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
        // Isolated from the environment like the merged config in `mirrord up`, whose
        // environment-derived settings come from the generated config rather than the patch.
        let context = ConfigContext::default().strict_env(true);
        issues.extend(
            check_layer_config(patch, context)?
                .into_iter()
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
    // serde also takes a struct written as an array of its field values, which no mirrord config
    // is meant to be; the schema rejects it.
    let serde_error = match serde_path_to_error::deserialize::<_, T>(value) {
        Ok(config) if value.is_object() => return Ok(Ok(config)),
        Ok(_) => None,
        Err(error) => Some(error),
    };

    let validator = schema
        .validator()
        .as_ref()
        .map_err(ValidateConfigError::InvalidSchema)?;
    let issues: Vec<_> = validator
        .iter_errors(value)
        .flat_map(|error| schema_issues(&error, &schema.raw))
        .collect();
    if issues.is_empty().not() {
        return Ok(Err(issues));
    }

    Ok(Err(vec![match serde_error {
        Some(error) => ConfigIssue {
            path: pointer_from_serde_path(error.path()),
            message: error.into_inner().to_string(),
            allowed_values: None,
        },
        None => file_issue("the config must be an object".to_owned()),
    }]))
}

/// Turns one schema violation into issues. An unknown-fields violation becomes one issue per
/// unknown field, pointing at the field itself.
fn schema_issues(error: &ValidationError<'_>, raw_schema: &Value) -> Vec<ConfigIssue> {
    let path = error.instance_path().as_str().to_owned();

    match error.kind() {
        ValidationErrorKind::AnyOf { context } | ValidationErrorKind::OneOfNotValid { context }
            if let Some(branch) = intended_branch(context, error)
                && is_enumeration(branch, error.instance_path()).not() =>
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
                // alternatives are the allowed values, plus maybe other forms of the setting.
                (
                    Some(_),
                    ValidationErrorKind::AnyOf { context }
                    | ValidationErrorKind::OneOfNotValid { context },
                ) => {
                    let mut types = Vec::new();
                    for branch in context {
                        expected_types(branch, error.instance_path(), &mut types);
                    }
                    let other_forms = types
                        .iter()
                        .filter(|json_type| **json_type != JsonType::Null)
                        .map(|json_type| format!("`{json_type}`"))
                        .collect::<Vec<_>>();
                    match other_forms.as_slice() {
                        [] => format!("{} is not an allowed value", error.instance()),
                        _ => format!(
                            "{} is not an allowed value; a value of type {} is also accepted",
                            error.instance(),
                            other_forms.join(" or ")
                        ),
                    }
                }
                (
                    None,
                    ValidationErrorKind::AnyOf { .. } | ValidationErrorKind::OneOfNotValid { .. },
                ) => match accepted_forms(error, raw_schema).as_slice() {
                    [] => error.to_string(),
                    forms => format!(
                        "{} matches none of the accepted forms: {}",
                        error.instance(),
                        forms.join("; ")
                    ),
                },
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

/// The one alternative of a failed `anyOf`/`oneOf` that the value was evidently meant to match:
/// the only alternative of the right JSON type or, among several alternatives taking an object,
/// the one that knows the most of the object's fields.
///
/// Every optional field is rendered by schemars as `anyOf: [<field schema>, {type: null}]`, and
/// config fields often accept a short form (a string) or a full object. Without this, a typo deep
/// inside any of them would only be reported as "not valid under any of the schemas" at the
/// top-most optional field.
fn intended_branch<'e>(
    branches: &'e [Vec<ValidationError<'static>>],
    error: &ValidationError<'_>,
) -> Option<&'e [ValidationError<'static>]> {
    let path = error.instance_path();
    let plausible: Vec<&Vec<ValidationError>> = branches
        .iter()
        .filter(|branch| is_wrong_type(branch, path).not())
        .collect();
    if let [branch] = plausible.as_slice() {
        return Some(branch);
    }

    let fields = error.instance().as_object()?.len();
    let mut ranked: Vec<(usize, &Vec<ValidationError>)> = plausible
        .into_iter()
        .map(|branch| (known_fields(branch, path, fields), branch))
        .collect();
    ranked.sort_by_key(|(known, _)| std::cmp::Reverse(*known));
    match ranked.as_slice() {
        [(best, branch), rest @ ..] if *best > 0 && rest.iter().all(|(known, _)| known < best) => {
            Some(branch)
        }
        _ => None,
    }
}

/// How many of the `fields` of the object at `path` an alternative knows: all of them unless its
/// errors name some as unknown.
fn known_fields(branch: &[ValidationError<'_>], path: &Location, fields: usize) -> usize {
    branch
        .iter()
        .filter(|error| error.instance_path() == path)
        .map(|error| match error.kind() {
            ValidationErrorKind::AdditionalProperties { unexpected } => {
                fields.saturating_sub(unexpected.len())
            }
            ValidationErrorKind::AnyOf { context }
            | ValidationErrorKind::OneOfNotValid { context } => context
                .iter()
                .filter(|branch| is_wrong_type(branch, path).not())
                .map(|branch| known_fields(branch, path, fields))
                .max()
                .unwrap_or(0),
            _ => fields,
        })
        .min()
        .unwrap_or(fields)
}

/// Whether the errors of an alternative show the value at `path` has the wrong JSON type for it:
/// the alternative takes another type, enumerates values of other types only, or is an
/// `anyOf`/`oneOf` of such alternatives.
fn is_wrong_type(branch: &[ValidationError<'_>], path: &Location) -> bool {
    branch.iter().any(|error| {
        let same_type = |value: &Value| {
            std::mem::discriminant(value) == std::mem::discriminant(error.instance().as_ref())
        };
        error.instance_path() == path
            && match error.kind() {
                ValidationErrorKind::Type { .. } => true,
                ValidationErrorKind::Constant { expected_value } => same_type(expected_value).not(),
                ValidationErrorKind::Enum { options } => options
                    .as_array()
                    .is_some_and(|options| options.iter().any(same_type).not()),
                ValidationErrorKind::AnyOf { context }
                | ValidationErrorKind::OneOfNotValid { context } => {
                    context.iter().all(|nested| is_wrong_type(nested, path))
                }
                _ => false,
            }
    })
}

/// Whether an alternative failed only because the value isn't one of the values it enumerates.
///
/// Such an alternative is not reported on its own even when it is the only one of the right JSON
/// type: for a setting that is either one of a few strings or an object (like `target: none` in a
/// `mirrord-up.yaml`), a string that isn't allowed may well have been meant as the object, and
/// the issue has to say that the object is accepted too.
fn is_enumeration(branch: &[ValidationError<'_>], path: &Location) -> bool {
    branch.iter().all(|error| {
        error.instance_path() == path
            && match error.kind() {
                ValidationErrorKind::Enum { .. } | ValidationErrorKind::Constant { .. } => true,
                ValidationErrorKind::AnyOf { context }
                | ValidationErrorKind::OneOfNotValid { context } => {
                    let mut plausible = context
                        .iter()
                        .filter(|branch| is_wrong_type(branch, path).not())
                        .peekable();
                    plausible.peek().is_some()
                        && plausible.all(|branch| is_enumeration(branch, path))
                }
                _ => false,
            }
    })
}

/// Collects the JSON types that `errors` show the value at `path` was expected to have, looking
/// into nested `anyOf`/`oneOf`s.
fn expected_types(errors: &[ValidationError<'_>], path: &Location, types: &mut Vec<JsonType>) {
    for error in errors.iter().filter(|error| error.instance_path() == path) {
        match error.kind() {
            ValidationErrorKind::Type { kind } => {
                let expected = match kind {
                    TypeKind::Single(json_type) => vec![*json_type],
                    TypeKind::Multiple(set) => set.iter().collect(),
                };
                for json_type in expected {
                    if types.contains(&json_type).not() {
                        types.push(json_type);
                    }
                }
            }
            ValidationErrorKind::AnyOf { context }
            | ValidationErrorKind::OneOfNotValid { context } => {
                for branch in context {
                    expected_types(branch, path, types);
                }
            }
            _ => {}
        }
    }
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

/// Describes the alternatives of a failed `anyOf`/`oneOf`, e.g. "a `string`" or "an object with
/// `local`", leaving out `null`.
fn accepted_forms(error: &ValidationError<'_>, raw_schema: &Value) -> Vec<String> {
    let Some(alternatives) = raw_schema
        .pointer(error.schema_path().as_str())
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };

    let mut forms = Vec::new();
    for Node { schema, .. } in expand(raw_schema, alternatives.iter().map(Node::root)).nodes {
        let form = if let Some(value) = schema.get("const") {
            Some(format!("`{value}`"))
        } else if let Some(values) = schema.get("enum").and_then(Value::as_array) {
            let values: Vec<String> = values.iter().map(|value| format!("`{value}`")).collect();
            Some(values.join(" or "))
        } else if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            let fields: Vec<String> = properties
                .keys()
                .map(|field| format!("`{field}`"))
                .collect();
            Some(format!("an object with {}", fields.join(", ")))
        } else {
            match schema.get("type") {
                Some(Value::String(json_type)) if json_type != "null" => {
                    Some(format!("a `{json_type}`"))
                }
                Some(Value::Array(types)) => {
                    let types: Vec<String> = types
                        .iter()
                        .filter_map(Value::as_str)
                        .filter(|json_type| *json_type != "null")
                        .map(|json_type| format!("a `{json_type}`"))
                        .collect();
                    (types.is_empty().not()).then(|| types.join(" or "))
                }
                _ => None,
            }
        };
        if let Some(form) = form
            && forms.contains(&form).not()
        {
            forms.push(form);
        }
    }
    forms
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
    use rstest::rstest;
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
        assert!(LAYER_SCHEMA.validator().is_ok());
        assert!(UP_SCHEMA.validator().is_ok());
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

    /// `connection` is a serde alias of `source`, which the schema doesn't know about.
    #[test]
    fn serde_alias() {
        let output = validate(
            ConfigFormat::MirrordJson,
            r#"{ "feature": { "db_branches": [ { "type": "turbopuffer", "connection": { "params": {
                "namespace": "TPUF_NAMESPACE",
                "api_key": "TURBOPUFFER_API_KEY",
                "region": { "env_var_name": "TURBOPUFFER_REGION", "value": "gcp-us-central1" }
            } } } ] } }"#,
        );
        assert!(output.valid, "{:?}", output.issues);
    }

    /// Passes the schema and deserializes, but `verify` rejects it.
    #[test]
    fn conflicting_http_filters() {
        let issue = single_issue(
            ConfigFormat::MirrordJson,
            r#"{ "feature": { "network": { "incoming": { "mode": "steal",
                "http_filter": { "header_filter": "a", "path_filter": "b" } } } } }"#,
        );
        assert!(
            issue.message.contains("multiple types of HTTP filter"),
            "{}",
            issue.message
        );
    }

    /// A missing target may still come from the command line, so it doesn't make the config
    /// targetless (which would conflict with `steal`).
    #[test]
    fn missing_target_not_final() {
        let output = validate(
            ConfigFormat::MirrordJson,
            r#"{ "feature": { "network": { "incoming": "steal" } } }"#,
        );
        assert!(output.valid, "{:?}", output.issues);
    }

    #[test]
    fn up_yaml_key_dependent_template() {
        let content = r#"
services:
  app:
    run:
      command: ["true"]
{% if key == "prod" %}    bogus: 1
{% endif %}"#;
        assert!(validate(ConfigFormat::MirrordUpYaml, content).valid);

        let mut output = validate_config(ValidateConfigArgs {
            format: ConfigFormat::MirrordUpYaml,
            content: content.to_owned(),
            key: Some("prod".to_owned()),
        })
        .unwrap();
        let issue = output.issues.pop().unwrap();
        assert!(output.issues.is_empty(), "{:?}", output.issues);
        assert_eq!(issue.path, "/services/app/bogus");
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

    /// Deserializes, but `verify` rejects the jq filter.
    #[test]
    fn up_yaml_config_patch_invalid_jq_filter() {
        let issue = single_issue(
            ConfigFormat::MirrordUpYaml,
            r#"
services:
  worker:
    config_patch:
      feature:
        split_queues:
          "*":
            queue_type: SQS
            jq_filter: "["
    run:
      command: ["echo"]
"#,
        );
        assert_eq!(issue.path, "/services/worker/config_patch");
        assert!(issue.message.contains("jq"), "{}", issue.message);
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

    /// Deserializes, but `mirrord up` only supports `run.directory` for `exec` services.
    #[test]
    fn up_yaml_container_run_directory() {
        let issue = single_issue(
            ConfigFormat::MirrordUpYaml,
            r#"
services:
  app:
    run:
      type: container
      directory: ./app
      command: ["docker", "run", "app"]
"#,
        );
        assert_eq!(issue.path, "/services/app/run/directory");
        assert!(issue.message.contains("type: exec"), "{}", issue.message);
    }

    /// `target` is `none` or a mapping, so a string other than `none` is reported along with the
    /// mapping it could have been.
    #[test]
    fn up_yaml_string_target() {
        let issue = single_issue(
            ConfigFormat::MirrordUpYaml,
            "services:\n  api:\n    target: deployment/api\n    run:\n      command: [\"true\"]\n",
        );
        assert_eq!(issue.path, "/services/api/target");
        assert_eq!(issue.allowed_values, Some(vec![json!("none")]));
        assert!(issue.message.contains("`object`"), "{}", issue.message);
    }

    /// A repeated field is reported at the field itself.
    #[rstest]
    #[case::top_level(
        r#"{ "target": "deployment/a", "target": "pod/b" }"#,
        "/target",
        "target"
    )]
    #[case::nested(
        r#"{ "feature": { "network": { "incoming": { "port_mapping": [[1, 2]], "port_mapping": [[3, 4]] } } } }"#,
        "/feature/network/incoming/port_mapping",
        "port_mapping"
    )]
    fn duplicate_field(#[case] content: &str, #[case] path: &str, #[case] field: &str) {
        let issue = single_issue(ConfigFormat::MirrordJson, content);
        assert_eq!(issue.path, path);
        assert!(
            issue
                .message
                .contains(&format!("duplicate field `{field}`")),
            "{}",
            issue.message
        );
    }

    /// A key given twice in a `mirrord-up.yaml` is rejected, as `mirrord up` rejects it, with a
    /// message for the author of the file rather than for the program parsing it.
    #[test]
    fn up_yaml_duplicate_key() {
        let issue = single_issue(
            ConfigFormat::MirrordUpYaml,
            "services:\n  api:\n    target: none\n    target: none\n    run:\n      command: [\"true\"]\n",
        );
        assert!(
            issue.message.contains("duplicate mapping key: target"),
            "{}",
            issue.message
        );
        assert!(issue.message.contains("line 4"), "{}", issue.message);
        assert_eq!(issue.path, "/services/api/target");
        assert!(
            issue.message.contains("DuplicateKeyPolicy").not(),
            "{}",
            issue.message
        );
    }

    #[rstest]
    #[case::mirrord_json(
        ConfigFormat::MirrordJson,
        r#"{ "feature": { "network": { "incoming": { "http_filter": { "header_filter": "([" } } } } }"#,
        "/feature/network/incoming/http_filter/header_filter"
    )]
    #[case::up_yaml(
        ConfigFormat::MirrordUpYaml,
        "services:\n  api:\n    http_filter:\n      header_filter: \"([\"\n    run:\n      command: [\"true\"]\n",
        "/services/api/http_filter/header_filter"
    )]
    fn invalid_http_filter_regex(
        #[case] format: ConfigFormat,
        #[case] content: &str,
        #[case] path: &str,
    ) {
        let issue = single_issue(format, content);
        assert_eq!(issue.path, path);
        assert!(issue.message.contains("`([`"), "{}", issue.message);
    }

    /// Both formats offer `target` as a string or a mapping, so the unknown field has to be found
    /// in the mapping.
    #[rstest]
    #[case::mirrord_json(
        ConfigFormat::MirrordJson,
        r#"{ "target": { "path": "deployment/api", "bogus": 1 } }"#,
        "/target/bogus"
    )]
    #[case::up_yaml(
        ConfigFormat::MirrordUpYaml,
        "services:\n  api:\n    target:\n      path: deployment/api\n      bogus: 1\n    run:\n      command: [\"true\"]\n",
        "/services/api/target/bogus"
    )]
    fn unknown_target_field(
        #[case] format: ConfigFormat,
        #[case] content: &str,
        #[case] path: &str,
    ) {
        let issue = single_issue(format, content);
        assert_eq!(issue.path, path);
        assert_eq!(issue.message, "unknown field `bogus`");
    }

    /// Conflicts point at the setting to change.
    #[rstest]
    #[case::http_filters(
        ConfigFormat::MirrordJson,
        r#"{ "feature": { "network": { "incoming": { "mode": "steal", "http_filter": { "header_filter": "a", "path_filter": "b" } } } } }"#,
        "/feature/network/incoming/http_filter"
    )]
    #[case::env_include_exclude(
        ConfigFormat::MirrordJson,
        r#"{ "feature": { "env": { "include": "A", "exclude": "B" } } }"#,
        "/feature/env"
    )]
    #[case::copy_target_targetless(
        ConfigFormat::MirrordJson,
        r#"{ "target": "targetless", "feature": { "copy_target": true } }"#,
        "/feature/copy_target"
    )]
    #[case::config_patch(
        ConfigFormat::MirrordUpYaml,
        "services:\n  api:\n    config_patch:\n      feature:\n        env:\n          include: A\n          exclude: B\n    run:\n      command: [\"true\"]\n",
        "/services/api/config_patch/feature/env"
    )]
    #[case::service_setting(
        ConfigFormat::MirrordUpYaml,
        "services:\n  api:\n    env:\n      include: A\n      exclude: B\n    run:\n      command: [\"true\"]\n",
        "/services/api/env"
    )]
    fn conflict_location(#[case] format: ConfigFormat, #[case] content: &str, #[case] path: &str) {
        let issue = single_issue(format, content);
        assert_eq!(issue.path, path);
        assert!(
            issue.message.starts_with("Conflicting configuration"),
            "{}",
            issue.message
        );
    }

    /// The reason a target path doesn't parse, rather than serde's "did not match any variant".
    #[rstest]
    #[case::simple(r#"{ "target": "banana/api" }"#, "/target")]
    #[case::advanced(
        r#"{ "target": { "path": "banana/api", "namespace": "default" } }"#,
        "/target/path"
    )]
    fn invalid_target_path(#[case] content: &str, #[case] path: &str) {
        let issue = single_issue(ConfigFormat::MirrordJson, content);
        assert_eq!(issue.path, path);
        assert_eq!(issue.message, "`banana/api` is not a valid target path");
        let formats = issue.allowed_values.unwrap();
        assert!(
            formats.contains(&json!(
                "deployment/{deployment-name}[/container/{container-name}]"
            )),
            "{formats:?}"
        );
    }

    /// A specific reason is kept, a malformed label target here.
    #[test]
    fn invalid_label_target() {
        let issue = single_issue(ConfigFormat::MirrordJson, r#"{ "target": "label/app" }"#);
        assert_eq!(issue.path, "/target");
        assert!(
            issue
                .message
                .starts_with("`label/app` is not a valid target path: Label target"),
            "{}",
            issue.message
        );
    }

    /// The duplicate key is found wherever and however it's written.
    #[rstest]
    #[case::flow(
        "services: {api: {target: none, target: none, run: {command: [x]}}}\n",
        "/services/api/target"
    )]
    #[case::in_config_patch(
        "services:\n  api:\n    config_patch:\n      feature:\n        env:\n          override:\n            A: x\n            A: y\n    run:\n      command: [x]\n",
        "/services/api/config_patch/feature/env/override/A"
    )]
    #[case::in_sequence(
        "services:\n  api:\n    run:\n      command: [{a: 1, a: 2}]\n",
        "/services/api/run/command/0/a"
    )]
    fn up_yaml_duplicate_key_path(#[case] content: &str, #[case] path: &str) {
        assert_eq!(
            single_issue(ConfigFormat::MirrordUpYaml, content).path,
            path
        );
    }

    /// An index-less `[]` stands for any entry, so the path stops at the list.
    #[rstest]
    #[case::field("startup_retry.max_ms", "/startup_retry/max_ms")]
    #[case::leading_dot(
        ".feature.network.incoming.tls_delivery.server_name",
        "/feature/network/incoming/tls_delivery/server_name"
    )]
    #[case::indexed(
        "feature.preview.config_mounts[0].payload",
        "/feature/preview/config_mounts/0/payload"
    )]
    #[case::any_entry("feature.db_branches[].copy.image", "/feature/db_branches")]
    #[case::env_var("MIRRORD_AGENT_TTL", "")]
    fn config_error_location(#[case] name: &'static str, #[case] path: &str) {
        let error = ConfigError::InvalidValue {
            name: name.into(),
            provided: String::new(),
            error: "invalid".into(),
        };
        assert_eq!(config_error_path(&error), path);
    }

    /// `mirrord up` ignores the `http_filter` of a service in `replace` mode.
    #[test]
    fn up_yaml_replace_mode_ignores_http_filter() {
        let output = validate(
            ConfigFormat::MirrordUpYaml,
            "services:\n  api:\n    default_mode: replace\n    http_filter:\n      header_filter: \"(\"\n    run:\n      command: [\"true\"]\n",
        );
        assert!(output.valid, "{:?}", output.issues);
    }

    #[test]
    fn accepted_forms() {
        let issue = single_issue(ConfigFormat::MirrordJson, r#"{ "agent": { "image": 5 } }"#);
        assert_eq!(issue.path, "/agent/image");
        assert!(issue.message.contains("a `string`"), "{}", issue.message);
        assert!(issue.message.contains("`registry`"), "{}", issue.message);
    }

    #[test]
    fn top_level_array() {
        let issue = single_issue(ConfigFormat::MirrordJson, "[]");
        assert_eq!(issue.path, "");
    }

    #[test]
    fn up_yaml_missing_services() {
        let issue = single_issue(ConfigFormat::MirrordUpYaml, "common: {}\n");
        assert_eq!(issue.path, "");
        assert!(issue.message.contains("services"), "{}", issue.message);
    }
}
