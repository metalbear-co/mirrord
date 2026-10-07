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

use std::{collections::HashSet, fmt, ops::Not, path::Path, str::FromStr};

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
    target::{TARGET_PATH_FORMATS, Target},
};
use mirrord_up::{SERVICE_LAYER_PATHS, ServiceError, ServiceMode, UpConfig, UpError};
use schemars::JsonSchema;
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{DeserializeOwned, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;
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
                    // The `Value` keeps the last of a key given twice, which `mirrord exec`
                    // rejects for the fields of the config.
                    let mut issues = duplicate_keys(&rendered);
                    issues.extend(check_layer_config(&value, context)?);
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
                let mut path = String::new();
                serde_saphyr::with_deserializer_from_str(&rendered, |deserializer| {
                    serde_path_to_error::deserialize(deserializer).map_err(|error| {
                        path = pointer_from_serde_path(error.path());
                        error.into_inner()
                    })
                })
                .map_err(|error| ConfigIssue {
                    // The parser reports a key given twice at the mapping that holds it.
                    path: match error.without_snippet() {
                        serde_saphyr::Error::DuplicateMappingKey { key: Some(key), .. } => {
                            format!("{path}/{}", escape_pointer_token(key))
                        }
                        _ => String::new(),
                    },
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
                    let (mut issues, config) = match check::<UpConfig>(&value, &UP_SCHEMA)? {
                        Ok(config) => {
                            let issues = config.verify().err().map(up_issue).into_iter().collect();
                            (issues, Some(config))
                        }
                        Err(issues) => (issues, None),
                    };
                    let setting_issues = check_services(&value, &issues)?;
                    match config.map(|config| config.verify_services(&key)) {
                        // A service whose settings or `config_patch` have issues of their own
                        // also fails to assemble; those issues already point at the culprit.
                        Some(Ok(errors)) => {
                            issues.extend(errors.into_iter().map(service_issue).filter(|issue| {
                                let service = format!("{}/", issue.path);
                                setting_issues
                                    .iter()
                                    .any(|setting| setting.path.starts_with(&service))
                                    .not()
                            }))
                        }
                        Some(Err(error)) => issues.push(file_issue(error.to_string())),
                        None => {}
                    }
                    issues.extend(setting_issues);
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
        ConfigError::InvalidTargetPath(_) => format!("`{target_path}` is not a valid target path"),
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

/// A key given twice in an object of a JSON document, at the second one.
///
/// Read from the text rather than from the config types, so it's found in any object, including
/// those of options with several forms, which serde would only report as matching none of them.
fn duplicate_keys(json: &str) -> Vec<ConfigIssue> {
    struct DuplicateKeys<'a> {
        path: String,
        issues: &'a mut Vec<ConfigIssue>,
    }

    impl<'de> DeserializeSeed<'de> for DuplicateKeys<'_> {
        type Value = ();

        fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
            deserializer.deserialize_any(self)
        }
    }

    impl<'de> Visitor<'de> for DuplicateKeys<'_> {
        type Value = ();

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("any JSON value")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
            let mut keys = HashSet::new();
            while let Some(key) = map.next_key::<String>()? {
                let path = format!("{}/{}", self.path, escape_pointer_token(&key));
                if keys.contains(&key) {
                    self.issues.push(ConfigIssue {
                        path: path.clone(),
                        message: format!("duplicate field `{key}`"),
                        allowed_values: None,
                    });
                }
                keys.insert(key);
                map.next_value_seed(DuplicateKeys {
                    path,
                    issues: &mut *self.issues,
                })?;
            }
            Ok(())
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
            let mut index = 0;
            while seq
                .next_element_seed(DuplicateKeys {
                    path: format!("{}/{index}", self.path),
                    issues: &mut *self.issues,
                })?
                .is_some()
            {
                index += 1;
            }
            Ok(())
        }

        fn visit_bool<E>(self, _: bool) -> Result<(), E> {
            Ok(())
        }

        fn visit_i64<E>(self, _: i64) -> Result<(), E> {
            Ok(())
        }

        fn visit_u64<E>(self, _: u64) -> Result<(), E> {
            Ok(())
        }

        fn visit_f64<E>(self, _: f64) -> Result<(), E> {
            Ok(())
        }

        fn visit_str<E>(self, _: &str) -> Result<(), E> {
            Ok(())
        }

        fn visit_unit<E>(self) -> Result<(), E> {
            Ok(())
        }
    }

    let mut issues = Vec::new();
    // The document parsed as a `Value` before, so reading it again doesn't fail.
    let _ = DuplicateKeys {
        path: String::new(),
        issues: &mut issues,
    }
    .deserialize(&mut serde_json::Deserializer::from_str(json));
    issues
}

/// An issue with a service as `mirrord up` assembles it, which comes from several of its settings
/// together, so it points at the service as a whole.
fn service_issue(error: ServiceError) -> ConfigIssue {
    ConfigIssue {
        path: format!("/services/{}", escape_pointer_token(error.service())),
        message: error.to_string(),
        allowed_values: None,
    }
}

/// An issue with the file as a whole: a template or syntax error that prevented reading it.
fn file_issue(message: String) -> ConfigIssue {
    ConfigIssue {
        path: String::new(),
        message,
        allowed_values: None,
    }
}

/// Runs the checks of a mirrord config on what `mirrord up` generates for every service of a
/// `mirrord-up.yaml`: the settings it copies into the mirrord config (per [`SERVICE_LAYER_PATHS`]),
/// such as the regexes of an `http_filter`, with the service's `config_patch` merged over them as
/// `mirrord up` merges it, since only the result has to be valid. The full generated config
/// depends on resolving targets in the cluster, so it can't be reproduced here; nearly every
/// mirrord config field is optional, so the settings and the patch are a valid fragment of it.
///
/// An issue in what the patch sets is reported in the patch, any other at the setting it comes
/// from. Left out are `default_mode`, as `mirrord up` translates its values, the settings that
/// need the cluster to resolve, like `target`, the `http_filter` of a service in `replace` mode,
/// which `mirrord up` ignores (unless `mirrord up --mode` overrides the mode, which a file can't
/// tell), and settings with an issue of their own in `issues` already.
fn check_services(
    up_config: &Value,
    issues: &[ConfigIssue],
) -> Result<Vec<ConfigIssue>, ValidateConfigError> {
    let Some(services) = up_config.get("services").and_then(Value::as_object) else {
        return Ok(Vec::new());
    };

    let mut service_issues = Vec::new();
    for (service, settings) in services {
        let service_pointer = format!("/services/{}", escape_pointer_token(service));
        let replace_mode = settings
            .get("default_mode")
            .and_then(|mode| ServiceMode::deserialize(mode).ok())
            == Some(ServiceMode::Replace);
        let mut layer_config = Value::Object(Default::default());
        let mut copied = Vec::new();
        for &(setting, layer_path) in SERVICE_LAYER_PATHS {
            if layer_path.starts_with("feature.").not()
                || setting == "default_mode"
                || (setting == "http_filter" && replace_mode)
            {
                continue;
            }
            let Some(value) = settings.get(setting) else {
                continue;
            };
            let up_pointer = format!("{service_pointer}/{setting}");
            if issues
                .iter()
                .any(|issue| issue.path.starts_with(&up_pointer))
            {
                continue;
            }
            let layer_pointer = dotted_to_pointer(layer_path);
            insert_at(&mut layer_config, &layer_pointer, value.clone());
            copied.push((layer_pointer, up_pointer));
        }
        let patch = settings.get("config_patch");
        if let Some(patch) = patch {
            json_patch::merge(&mut layer_config, patch);
        } else if copied.is_empty() {
            continue;
        }

        let patch_pointer = format!("{service_pointer}/config_patch");
        // Isolated from the environment like the merged config in `mirrord up`, whose
        // environment-derived settings come from the generated config rather than the patch.
        let context = ConfigContext::default().strict_env(true);
        service_issues.extend(check_layer_config(&layer_config, context)?.into_iter().map(
            |issue| {
                // An issue about the whole config is the patch's when nothing else is in it.
                let in_patch = patch.is_some_and(|patch| match issue.path.as_str() {
                    "" => patch.is_object().not() || copied.is_empty(),
                    path => patch.pointer(path).is_some(),
                });
                let path = if in_patch {
                    format!("{patch_pointer}{}", issue.path)
                } else {
                    copied
                        .iter()
                        .find_map(|(layer_pointer, up_pointer)| {
                            let rest = issue.path.strip_prefix(layer_pointer.as_str())?;
                            Some(format!("{up_pointer}{rest}"))
                        })
                        .unwrap_or_else(|| service_pointer.clone())
                };
                ConfigIssue { path, ..issue }
            },
        ));
    }

    Ok(service_issues)
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
        .filter(|branch| mismatches_tag(branch, path).not())
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

/// Whether an alternative rejects the value of one of the object's fields at `path` for not being
/// one it enumerates, like the `type` of a database branch: such a field tells the alternatives
/// apart, and the object was meant for another one.
fn mismatches_tag(branch: &[ValidationError<'_>], path: &Location) -> bool {
    branch.iter().any(|error| match error.kind() {
        ValidationErrorKind::Constant { .. } | ValidationErrorKind::Enum { .. } => error
            .instance_path()
            .as_str()
            .strip_prefix(path.as_str())
            .and_then(|field| field.strip_prefix('/'))
            .is_some_and(|field| field.contains('/').not()),
        ValidationErrorKind::AnyOf { context } | ValidationErrorKind::OneOfNotValid { context } => {
            context
                .iter()
                .all(|alternative| mismatches_tag(alternative, path))
        }
        _ => false,
    })
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
mod tests;
