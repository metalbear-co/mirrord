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
use tokio::fs;

use super::{default_path, initialize_empty_json_at_path, update_at_path, update_at_path_strict};
use crate::config::global_config::{
    ExportGlobalConfigArgs, GlobalConfigArgs, GlobalConfigCommand, ImportGlobalConfigArgs,
    SetGlobalConfigArgs, UnsetGlobalConfigArgs,
};

/// "~/.mirrord/mirrord.json"
static GLOBAL_CONFIG_PATH: LazyLock<PathBuf> = LazyLock::new(|| default_path("mirrord.json"));

/// A regular mirrord configuration loaded from the global config path.
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
        // LayerConfig::resolve needs an existing file. Initialization deliberately avoids parsing
        // existing contents so regular config templating remains available on this path.
        initialize_empty_json_at_path(path).await?;

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

    /// Parses and strictly validates a complete sparse global configuration document.
    pub(crate) fn from_strict_json(
        contents: &str,
    ) -> Result<BTreeMap<String, Value>, GlobalConfigValidationError> {
        let value: Value = serde_json::from_str(contents)?;
        Self::validate_value(value.clone())?;
        Ok(serde_json::from_value(value)?)
    }

    /// Records that an operator session succeeded.
    pub(crate) async fn remember_operator() -> io::Result<()> {
        Self::remember_operator_at_path(GLOBAL_CONFIG_PATH.as_path()).await
    }

    async fn remember_operator_at_path(path: &Path) -> io::Result<()> {
        update_at_path_strict(path, |config: &mut LayerFileConfig| {
            config.operator = Some(true);
            Ok::<_, io::Error>(())
        })
        .await?;
        Ok(())
    }

    /// Applies the global config to the project config without overriding explicit project, CLI,
    /// or environment values.
    pub(crate) fn apply_to(&self, project_config: &mut LayerConfig) {
        project_config.operator = project_config.operator.or(self.config.operator);
    }
}

/// Errors returned by `mirrord global-config`.
#[derive(Debug, Diagnostic, Error)]
pub(crate) enum GlobalConfigError {
    /// Reading or updating `~/.mirrord/mirrord.json` failed.
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
        GlobalConfigCommand::Export(args) => export(args).await,
        GlobalConfigCommand::Import(args) => import_config(args).await,
        GlobalConfigCommand::Set(args) => set(args).await,
        GlobalConfigCommand::Unset(args) => unset(args).await,
    }
}

async fn show() -> Result<(), GlobalConfigError> {
    export(ExportGlobalConfigArgs { file: None }).await
}

async fn export(args: ExportGlobalConfigArgs) -> Result<(), GlobalConfigError> {
    let config = GlobalConfig::update(|_| Ok::<_, GlobalConfigError>(())).await?;
    write_export(&config, args.file.as_deref()).await?;
    Ok(())
}

async fn import_config(args: ImportGlobalConfigArgs) -> Result<(), GlobalConfigError> {
    let contents = import_contents(args).await?;
    let imported = GlobalConfig::from_strict_json(&contents)?;

    GlobalConfig::update(move |config| {
        *config = imported;
        Ok::<_, GlobalConfigError>(())
    })
    .await?;
    Ok(())
}

fn export_json(config: &BTreeMap<String, Value>) -> Result<String, serde_json::Error> {
    let mut contents = serde_json::to_string_pretty(config)?;
    contents.push('\n');
    Ok(contents)
}

async fn write_export(
    config: &BTreeMap<String, Value>,
    file: Option<&Path>,
) -> Result<(), GlobalConfigError> {
    let contents = export_json(config)?;

    if let Some(path) = file {
        fs::write(path, contents).await?;
    } else {
        print!("{contents}");
    }

    Ok(())
}

async fn import_contents(args: ImportGlobalConfigArgs) -> Result<String, GlobalConfigError> {
    match (args.json, args.file) {
        (Some(contents), None) => Ok(contents),
        (None, Some(path)) => Ok(fs::read_to_string(path).await?),
        _ => unreachable!("clap requires exactly one global-config import source"),
    }
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
        target::Target,
    };
    use serde_json::Value;
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

    async fn remember_operator_round_trip(original: Value) -> Value {
        let directory = tempdir().unwrap();
        let path = directory.path().join("mirrord.json");
        fs::write(&path, serde_json::to_vec(&original).unwrap())
            .await
            .unwrap();

        GlobalConfig::remember_operator_at_path(&path)
            .await
            .unwrap();

        let stored: Value = serde_json::from_slice(&fs::read(path).await.unwrap()).unwrap();
        let _: LayerFileConfig = serde_json::from_value(stored.clone()).unwrap();
        assert!(
            !contains_null(&stored),
            "serialized config contains null: {stored}"
        );

        stored
    }

    fn contains_null(value: &Value) -> bool {
        match value {
            Value::Null => true,
            Value::Array(values) => values.iter().any(contains_null),
            Value::Object(values) => values.values().any(contains_null),
            _ => false,
        }
    }

    #[tokio::test]
    async fn creates_empty_global_config() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("mirrord.json");

        let config = GlobalConfig::from_path(&path).await.unwrap();

        assert_eq!(config.config.operator, None);
        assert_eq!(fs::read(path).await.unwrap(), br#"{}"#);
    }

    #[tokio::test]
    async fn stores_operator_in_regular_mirrord_config() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("mirrord.json");
        fs::write(
            &path,
            br#"{"feature":{"env":{"include":"FOO"}},"telemetry":false}"#,
        )
        .await
        .unwrap();

        GlobalConfig::remember_operator_at_path(&path)
            .await
            .unwrap();
        let global_config = GlobalConfig::from_path(&path).await.unwrap();
        let stored: Value = serde_json::from_slice(&fs::read(path).await.unwrap()).unwrap();

        assert_eq!(global_config.config.operator, Some(true));
        assert!(!global_config.config.telemetry);
        assert_eq!(
            stored,
            serde_json::json!({
                "feature": {"env": {"include": "FOO"}},
                "operator": true,
                "telemetry": false,
            })
        );
    }

    #[tokio::test]
    async fn remember_operator_preserves_targetless_shorthand() {
        let stored = remember_operator_round_trip(serde_json::json!({
            "target": "targetless"
        }))
        .await;

        assert_eq!(
            stored,
            serde_json::json!({
                "operator": true,
                "target": "targetless",
            })
        );

        let file_config: LayerFileConfig = serde_json::from_value(stored).unwrap();
        let mut context = ConfigContext::default().strict_env(true);
        let resolved = file_config.generate_config(&mut context).unwrap();

        assert_eq!(resolved.operator, Some(true));
        assert_eq!(resolved.target.path, Some(Target::Targetless));
    }

    #[tokio::test]
    async fn remember_operator_preserves_namespace_only_target() {
        let stored = remember_operator_round_trip(serde_json::json!({
            "target": {"namespace": "bear-namespace"}
        }))
        .await;

        assert_eq!(
            stored,
            serde_json::json!({
                "operator": true,
                "target": {"namespace": "bear-namespace"},
            })
        );
    }

    #[tokio::test]
    async fn remember_operator_preserves_targetless_with_namespace() {
        let stored = remember_operator_round_trip(serde_json::json!({
            "target": {
                "path": "targetless",
                "namespace": "bear-namespace",
            }
        }))
        .await;

        assert_eq!(
            stored,
            serde_json::json!({
                "operator": true,
                "target": {
                    "path": "targetless",
                    "namespace": "bear-namespace",
                },
            })
        );
    }

    #[tokio::test]
    async fn remember_operator_preserves_omitted_target() {
        let stored = remember_operator_round_trip(serde_json::json!({
            "telemetry": false
        }))
        .await;

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
    fn exported_json_is_sparse_pretty_json_with_trailing_newline() {
        let config =
            GlobalConfig::from_strict_json(r#"{"kube_context":"wawel","operator":true}"#).unwrap();

        let contents = export_json(&config).unwrap();

        assert_eq!(
            contents,
            "{\n  \"kube_context\": \"wawel\",\n  \"operator\": true\n}\n"
        );
    }

    #[tokio::test]
    async fn file_export_overwrites_existing_contents() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("global-config.json");
        fs::write(&path, "stale contents").await.unwrap();
        let config = GlobalConfig::from_strict_json(r#"{"operator":true}"#).unwrap();

        write_export(&config, Some(&path)).await.unwrap();

        assert_eq!(
            fs::read_to_string(path).await.unwrap(),
            export_json(&config).unwrap()
        );
    }

    #[tokio::test]
    async fn import_reads_json_argument() {
        let contents = import_contents(ImportGlobalConfigArgs {
            json: Some(r#"{"kube_context":"wawel"}"#.to_owned()),
            file: None,
        })
        .await
        .unwrap();

        assert_eq!(contents, r#"{"kube_context":"wawel"}"#);
    }

    #[tokio::test]
    async fn import_reads_json_file() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("global-config.json");
        let expected = r#"{"operator":true}"#;
        fs::write(&path, expected).await.unwrap();

        let contents = import_contents(ImportGlobalConfigArgs {
            json: None,
            file: Some(path),
        })
        .await
        .unwrap();

        assert_eq!(contents, expected);
    }

    #[test]
    fn exported_global_config_round_trips_through_strict_import() {
        let expected = GlobalConfig::from_strict_json(
            r#"{"kube_context":"wawel","operator":true,"telemetry":false}"#,
        )
        .unwrap();

        let exported = export_json(&expected).unwrap();
        let imported = GlobalConfig::from_strict_json(&exported).unwrap();

        assert_eq!(imported, expected);
    }

    #[test]
    fn importing_rejects_unknown_fields() {
        let error = GlobalConfig::from_strict_json(r#"{"operator":true,"typo":true}"#).unwrap_err();

        assert!(error.to_string().contains("typo"));
    }

    #[test]
    fn importing_rejects_invalid_shapes() {
        let incorrect_type = GlobalConfig::from_strict_json(r#"{"kube_context":7}"#).unwrap_err();
        let non_object = GlobalConfig::from_strict_json(r#"["wawel"]"#).unwrap_err();

        assert!(incorrect_type.to_string().contains("invalid type"));
        assert!(
            non_object
                .to_string()
                .contains("expected a global configuration JSON object")
        );
        assert!(GlobalConfig::from_strict_json(r#"{"operator":true"#).is_err());
    }

    #[tokio::test]
    async fn importing_replaces_document_without_resolved_defaults() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("global-mirrord.json");
        fs::write(&path, br#"{"telemetry":false}"#).await.unwrap();
        let imported = GlobalConfig::from_strict_json(r#"{"operator":true}"#).unwrap();

        update_at_path(&path, move |config: &mut BTreeMap<String, Value>| {
            *config = imported;
            Ok::<_, io::Error>(())
        })
        .await
        .unwrap();

        assert_eq!(fs::read(path).await.unwrap(), br#"{"operator":true}"#);
    }

    #[tokio::test]
    async fn remember_operator_preserves_nested_configs() {
        let original = serde_json::json!({
            "agent": {"image": {"registry": "example.com/mirrord"}},
            "feature": {
                "copy_target": {"scale_down": true},
                "fs": {"read_only": ".*\\.json$"},
                "network": {"incoming": {"ports": [80]}},
            },
        });

        let stored = remember_operator_round_trip(original.clone()).await;
        let mut expected = original;
        expected
            .as_object_mut()
            .unwrap()
            .insert("operator".to_owned(), Value::Bool(true));

        assert_eq!(stored, expected);
    }

    #[tokio::test]
    async fn remember_operator_omits_explicit_null_optional_configs() {
        let stored = remember_operator_round_trip(serde_json::json!({
            "agent": {"security_context": null},
            "feature": {"db_branches": null},
        }))
        .await;

        assert_eq!(
            stored,
            serde_json::json!({
                "agent": {},
                "feature": {},
                "operator": true,
            })
        );
    }

    #[tokio::test]
    async fn remember_operator_preserves_shorthand_configs() {
        let stored = remember_operator_round_trip(serde_json::json!({
            "agent": {"image": "example.com/mirrord:latest"},
            "feature": {
                "copy_target": true,
                "env": {"include": "FOO"},
                "fs": "read",
                "network": {"incoming": "steal"},
            },
            "target": "pod/bear-pod",
        }))
        .await;

        assert_eq!(
            stored,
            serde_json::json!({
                "agent": {"image": "example.com/mirrord:latest"},
                "feature": {
                    "copy_target": true,
                    "env": {"include": "FOO"},
                    "fs": "read",
                    "network": {"incoming": "steal"},
                },
                "operator": true,
                "target": {"pod": "bear-pod"},
            })
        );
    }

    #[tokio::test]
    async fn invalid_global_config_errors_without_changing_contents() {
        for original in [
            br#"{"telemetry":"invalid"}"#.as_slice(),
            br#"{"unknown":true}"#,
            br#"{"telemetry": "unfinished""#,
        ] {
            let directory = tempdir().unwrap();
            let path = directory.path().join("mirrord.json");
            fs::write(&path, original).await.unwrap();

            assert!(GlobalConfig::from_path(&path).await.is_err());
            assert_eq!(fs::read(&path).await.unwrap(), original);

            assert!(
                GlobalConfig::remember_operator_at_path(&path)
                    .await
                    .is_err()
            );
            assert_eq!(fs::read(path).await.unwrap(), original);
        }
    }

    #[tokio::test]
    async fn loading_templated_config_uses_regular_resolution_without_rewriting() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("mirrord.json");
        let original = br#"{
  "telemetry": {{ false }}
}"#;
        fs::write(&path, original).await.unwrap();

        let config = GlobalConfig::from_path(&path).await.unwrap();

        assert!(!config.config.telemetry);
        assert_eq!(fs::read(&path).await.unwrap(), original);

        assert!(
            GlobalConfig::remember_operator_at_path(&path)
                .await
                .is_err()
        );
        assert_eq!(fs::read(path).await.unwrap(), original);
    }

    #[tokio::test]
    async fn storing_operator_omits_unset_fields() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("mirrord.json");

        GlobalConfig::remember_operator_at_path(&path)
            .await
            .unwrap();

        assert_eq!(fs::read(path).await.unwrap(), br#"{"operator":true}"#);
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

        global_config.apply_to(&mut project_config);

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

        global_config.apply_to(&mut project_config);

        assert_eq!(project_config.operator, Some(false));
    }
}
