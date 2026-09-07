use std::{
    collections::BTreeMap,
    io,
    ops::Not,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use jsonptr::{Assign, Delete, Pointer, PointerBuf};
use miette::Diagnostic;
use mirrord_config::{
    LayerConfig, LayerFileConfig,
    config::{ConfigContext, ConfigError, MirrordConfig},
};
use serde::de::IntoDeserializer;
use serde_json::Value;
use thiserror::Error;

use super::{default_path, update_at_path};
use crate::config::global_config::{
    GlobalConfigArgs, GlobalConfigCommand, SetGlobalConfigArgs, UnsetGlobalConfigArgs,
};

/// "~/.mirrord/global-mirrord.json"
static GLOBAL_CONFIG_PATH: LazyLock<PathBuf> =
    LazyLock::new(|| default_path("global-mirrord.json"));

/// A regular mirrord configuration loaded from the user-wide config path.
#[derive(Debug)]
pub(crate) struct GlobalConfig {
    config: LayerConfig,
}

/// Invalid global configuration supplied through a strict CLI input boundary.
#[derive(Debug, Error)]
pub(crate) enum GlobalConfigValidationError {
    /// The input does not match the serialized shape of a regular mirrord configuration.
    #[error("Invalid global configuration: {0}")]
    Json(#[from] serde_json::Error),

    /// Persistent data remains forward-compatible, but CLI input must not silently ignore typos.
    #[error("Unknown global configuration field `{0}`")]
    UnknownField(String),

    /// The candidate could be parsed but could not be resolved as a regular mirrord config.
    #[error("Invalid global configuration: {0}")]
    Config(#[from] ConfigError),

    /// A complete configuration is always represented by a JSON object.
    #[error("expected a global configuration JSON object")]
    ExpectedObject,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        let mut context = ConfigContext::default().strict_env(true);
        let config = LayerConfig::resolve(&mut context)
            .expect("the default mirrord configuration should always resolve");

        Self { config }
    }
}

impl GlobalConfig {
    /// Creates [`GlobalConfig`] from the default file path.
    pub(crate) async fn from_default_path() -> io::Result<Self> {
        Self::from_path(GLOBAL_CONFIG_PATH.as_path()).await
    }

    async fn from_path(path: &Path) -> io::Result<Self> {
        // LayerConfig::resolve needs an existing file, so use the shared locked, atomic update
        // path to initialize it and recover invalid JSON as an empty object. A raw map preserves
        // arbitrary fields without persisting resolved LayerConfig defaults.
        let _: BTreeMap<String, Value> = update_at_path(path, |_| Ok::<_, io::Error>(())).await?;

        let mut context = ConfigContext::default()
            .override_env(LayerConfig::FILE_PATH_ENV, path)
            .strict_env(true);
        let config = LayerConfig::resolve(&mut context).map_err(io::Error::other)?;

        Ok(Self { config })
    }

    /// Atomically updates the sparse global configuration document at the default path.
    pub(crate) async fn update<E>(
        update: impl FnOnce(&mut BTreeMap<String, Value>) -> Result<(), E> + Send + 'static,
    ) -> Result<BTreeMap<String, Value>, E>
    where
        E: From<io::Error> + Send + 'static,
    {
        update_at_path(GLOBAL_CONFIG_PATH.as_path(), update).await
    }

    /// Validates a candidate as a regular unresolved mirrord file configuration.
    pub(crate) fn validate_value(value: Value) -> Result<(), GlobalConfigValidationError> {
        if value.is_object().not() {
            return Err(GlobalConfigValidationError::ExpectedObject);
        }

        let mut unknown_field = None;
        let config: LayerFileConfig =
            serde_ignored::deserialize(value.into_deserializer(), |path| {
                unknown_field.get_or_insert_with(|| path.to_string());
            })?;

        if let Some(path) = unknown_field {
            return Err(GlobalConfigValidationError::UnknownField(path));
        }

        let mut context = ConfigContext::default().strict_env(true);
        config.generate_config(&mut context)?;
        Ok(())
    }

    /// Records that an operator session succeeded.
    pub(crate) async fn remember_operator() -> io::Result<()> {
        Self::remember_operator_at_path(GLOBAL_CONFIG_PATH.as_path()).await
    }

    async fn remember_operator_at_path(path: &Path) -> io::Result<()> {
        update_at_path(path, |config: &mut BTreeMap<String, Value>| {
            config.insert("operator".to_owned(), Value::Bool(true));
            Ok::<_, io::Error>(())
        })
        .await?;
        Ok(())
    }

    /// Merges the global config into the project config without overriding explicit project, CLI,
    /// or environment values.
    ///
    /// Only `operator` participates in this merge until another global setting is needed.
    pub(crate) fn merge(&self, project_config: &mut LayerConfig) {
        project_config.operator = project_config.operator.or(self.config.operator);
    }
}

/// Errors returned by `mirrord global-config`.
#[derive(Debug, Diagnostic, Error)]
pub(crate) enum GlobalConfigError {
    /// Reading or updating `~/.mirrord/global-mirrord.json` failed.
    #[error("Failed accessing global mirrord configuration: {0}")]
    Io(#[from] io::Error),

    /// Serializing the global configuration failed.
    #[error("Failed processing global mirrord configuration JSON: {0}")]
    Json(#[from] serde_json::Error),

    /// The resulting document is not a valid regular mirrord configuration.
    #[error(transparent)]
    Validation(#[from] GlobalConfigValidationError),

    /// A set argument omitted its assignment separator.
    #[error("Invalid global configuration assignment `{0}`; expected `/json/pointer=value`")]
    InvalidAssignment(String),

    /// A path does not use valid, non-root JSON Pointer syntax.
    #[error("Invalid JSON Pointer `{pointer}`: {message}")]
    InvalidPointer {
        /// Pointer supplied by the user.
        pointer: String,
        /// Reason the pointer cannot be used.
        message: String,
    },

    /// A pointer cannot be applied to the current document.
    #[error("Cannot update global configuration at `{pointer}`: {message}")]
    Mutation {
        /// Pointer supplied by the user.
        pointer: String,
        /// Reason the mutation cannot be applied.
        message: String,
    },
}

impl GlobalConfigError {
    fn invalid_pointer(pointer: &str, message: impl Into<String>) -> Self {
        Self::InvalidPointer {
            pointer: pointer.to_owned(),
            message: message.into(),
        }
    }

    fn mutation_error(pointer: &str, message: impl Into<String>) -> Self {
        Self::Mutation {
            pointer: pointer.to_owned(),
            message: message.into(),
        }
    }
}

#[derive(Debug)]
struct Assignment {
    pointer: PointerBuf,
    value: Value,
}

/// Handles all `mirrord global-config` subcommands.
pub(crate) async fn global_config_command(args: GlobalConfigArgs) -> Result<(), GlobalConfigError> {
    match args.command {
        GlobalConfigCommand::Show => show().await,
        GlobalConfigCommand::Set(args) => set(args).await,
        GlobalConfigCommand::Unset(args) => unset(args).await,
    }
}

async fn show() -> Result<(), GlobalConfigError> {
    let config = GlobalConfig::update(|_| Ok::<_, GlobalConfigError>(())).await?;
    println!("{}", serde_json::to_string_pretty(&config)?);
    Ok(())
}

async fn set(args: SetGlobalConfigArgs) -> Result<(), GlobalConfigError> {
    let assignments = args
        .assignments
        .into_iter()
        .map(parse_assignment)
        .collect::<Result<Vec<_>, _>>()?;

    GlobalConfig::update(move |config| apply_assignments(config, assignments)).await?;

    Ok(())
}

async fn unset(args: UnsetGlobalConfigArgs) -> Result<(), GlobalConfigError> {
    let pointers = args
        .pointers
        .into_iter()
        .map(parse_pointer)
        .collect::<Result<Vec<_>, _>>()?;

    GlobalConfig::update(move |config| apply_unsets(config, pointers)).await?;

    Ok(())
}

fn apply_assignments(
    config: &mut BTreeMap<String, Value>,
    assignments: Vec<Assignment>,
) -> Result<(), GlobalConfigError> {
    let mut candidate = serde_json::to_value(&*config)?;
    for assignment in assignments {
        set_pointer(&mut candidate, &assignment.pointer, assignment.value)?;
    }
    GlobalConfig::validate_value(candidate.clone())?;
    *config = serde_json::from_value(candidate)?;
    Ok(())
}

fn apply_unsets(
    config: &mut BTreeMap<String, Value>,
    pointers: Vec<PointerBuf>,
) -> Result<(), GlobalConfigError> {
    let mut candidate = serde_json::to_value(&*config)?;
    for pointer in pointers {
        unset_pointer(&mut candidate, &pointer)?;
    }
    GlobalConfig::validate_value(candidate.clone())?;
    *config = serde_json::from_value(candidate)?;
    Ok(())
}

fn parse_assignment(assignment: String) -> Result<Assignment, GlobalConfigError> {
    let Some((pointer, raw_value)) = assignment.split_once('=') else {
        return Err(GlobalConfigError::InvalidAssignment(assignment));
    };
    let pointer = parse_pointer(pointer.to_owned())?;
    let value =
        serde_json::from_str(raw_value).unwrap_or_else(|_| Value::String(raw_value.to_owned()));

    Ok(Assignment { pointer, value })
}

fn parse_pointer(pointer: String) -> Result<PointerBuf, GlobalConfigError> {
    let parsed = PointerBuf::parse(pointer)
        .map_err(|error| GlobalConfigError::invalid_pointer(error.subject(), error.to_string()))?;
    if parsed.is_root() {
        return Err(GlobalConfigError::invalid_pointer(
            parsed.as_str(),
            "the document root cannot be changed",
        ));
    }

    Ok(parsed)
}

fn set_pointer(
    document: &mut Value,
    pointer: &Pointer,
    new_value: Value,
) -> Result<(), GlobalConfigError> {
    document
        .assign(pointer, new_value)
        .map(|_| ())
        .map_err(|error| GlobalConfigError::mutation_error(pointer.as_str(), error.to_string()))
}

fn unset_pointer(document: &mut Value, pointer: &Pointer) -> Result<(), GlobalConfigError> {
    document
        .delete(pointer)
        .map(|_| ())
        .ok_or_else(|| GlobalConfigError::mutation_error(pointer.as_str(), "value does not exist"))
}

#[cfg(test)]
mod tests {
    use mirrord_config::{
        LayerFileConfig,
        config::{ConfigContext, MirrordConfig},
    };
    use tempfile::tempdir;
    use tokio::fs;

    use super::*;

    fn apply_set(
        config: &BTreeMap<String, Value>,
        assignments: &[&str],
    ) -> Result<BTreeMap<String, Value>, GlobalConfigError> {
        let assignments = assignments
            .iter()
            .map(|assignment| parse_assignment((*assignment).to_owned()))
            .collect::<Result<Vec<_>, _>>()?;
        let mut updated = config.clone();
        apply_assignments(&mut updated, assignments)?;
        Ok(updated)
    }

    fn apply_unset(
        config: &BTreeMap<String, Value>,
        pointers: &[&str],
    ) -> Result<BTreeMap<String, Value>, GlobalConfigError> {
        let pointers = pointers
            .iter()
            .map(|pointer| parse_pointer((*pointer).to_owned()))
            .collect::<Result<Vec<_>, _>>()?;
        let mut updated = config.clone();
        apply_unsets(&mut updated, pointers)?;
        Ok(updated)
    }

    #[tokio::test]
    async fn creates_empty_global_config() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("global-mirrord.json");

        let config = GlobalConfig::from_path(&path).await.unwrap();

        assert_eq!(config.config.operator, None);
        assert_eq!(fs::read(path).await.unwrap(), br#"{}"#);
    }

    #[tokio::test]
    async fn stores_operator_in_regular_mirrord_config() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("global-mirrord.json");
        fs::write(&path, br#"{"telemetry":false}"#).await.unwrap();

        GlobalConfig::remember_operator_at_path(&path)
            .await
            .unwrap();
        let global_config = GlobalConfig::from_path(&path).await.unwrap();
        let stored: Value = serde_json::from_slice(&fs::read(path).await.unwrap()).unwrap();

        assert_eq!(global_config.config.operator, Some(true));
        assert!(!global_config.config.telemetry);
        assert_eq!(
            stored,
            serde_json::json!({"operator": true, "telemetry": false})
        );
    }

    #[test]
    fn set_updates_sparse_regular_config() {
        let config =
            apply_set(&BTreeMap::new(), &["/kube_context=wawel", "/operator=true"]).unwrap();

        assert_eq!(
            serde_json::to_value(config).unwrap(),
            serde_json::json!({"kube_context": "wawel", "operator": true})
        );
    }

    #[test]
    fn set_preserves_equals_signs_in_plain_string_values() {
        let config = apply_set(&BTreeMap::new(), &["/kube_context=wawel=malbork"]).unwrap();

        assert_eq!(
            config.get("kube_context"),
            Some(&Value::String("wawel=malbork".to_owned()))
        );
    }

    #[test]
    fn set_accepts_quoted_string_that_looks_like_boolean() {
        let assignment = parse_assignment(r#"/kube_context="true""#.to_owned()).unwrap();

        assert_eq!(assignment.value, Value::String("true".to_owned()));
    }

    #[test]
    fn set_rejects_unknown_regular_config_field() {
        let error = apply_set(&BTreeMap::new(), &["/booga=true"]).unwrap_err();

        assert!(error.to_string().contains("booga"));
    }

    #[test]
    fn set_rejects_value_with_wrong_type() {
        let error = apply_set(&BTreeMap::new(), &["/operator=wedel"]).unwrap_err();

        assert!(error.to_string().contains("boolean"));
    }

    #[test]
    fn unset_removes_values_without_persisting_resolved_defaults() {
        let configured =
            apply_set(&BTreeMap::new(), &["/kube_context=wawel", "/operator=true"]).unwrap();
        let config = apply_unset(&configured, &["/kube_context"]).unwrap();

        assert_eq!(
            serde_json::to_value(config).unwrap(),
            serde_json::json!({"operator": true})
        );
    }

    #[test]
    fn unset_rejects_missing_value() {
        let error = apply_unset(&BTreeMap::new(), &["/operator"]).unwrap_err();

        assert!(error.to_string().contains("does not exist"));
    }

    #[test]
    fn mutation_rejects_malformed_json_pointer() {
        let error = apply_set(&BTreeMap::new(), &["operator=true"]).unwrap_err();

        assert!(error.to_string().contains("does not start with a slash"));
    }

    #[test]
    fn global_operator_applies_when_project_does_not_set_it() {
        let mut context = ConfigContext::default().strict_env(true);
        let mut project_config = LayerFileConfig::default()
            .generate_config(&mut context)
            .unwrap();
        let global_layer_config = LayerFileConfig {
            operator: Some(true),
            ..Default::default()
        }
        .generate_config(&mut context)
        .unwrap();
        let global_config = GlobalConfig {
            config: global_layer_config,
        };

        global_config.merge(&mut project_config);

        assert_eq!(project_config.operator, Some(true));
    }

    #[test]
    fn project_operator_overrides_global_operator() {
        let mut context = ConfigContext::default().strict_env(true);
        let mut project_config = LayerFileConfig {
            operator: Some(false),
            ..Default::default()
        }
        .generate_config(&mut context)
        .unwrap();
        let global_layer_config = LayerFileConfig {
            operator: Some(true),
            ..Default::default()
        }
        .generate_config(&mut context)
        .unwrap();
        let global_config = GlobalConfig {
            config: global_layer_config,
        };

        global_config.merge(&mut project_config);

        assert_eq!(project_config.operator, Some(false));
    }
}
