use std::{
    collections::BTreeMap,
    io,
    ops::Not,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use jsonptr::{Assign, Delete, PointerBuf};
use miette::Diagnostic;
use mirrord_config::{
    LayerConfig, LayerFileConfig,
    config::{ConfigContext, ConfigError, MirrordConfig},
};
use serde::de::IntoDeserializer;
use serde_json::Value;
use thiserror::Error;

use super::{default_path, initialize_empty_json_at_path, update_at_path_strict};
use crate::config::global_config::{
    GlobalConfigArgs, GlobalConfigCommand, SetGlobalConfigArgs, UnsetGlobalConfigArgs,
};

/// "~/.mirrord/mirrord.json"
static GLOBAL_CONFIG_PATH: LazyLock<PathBuf> = LazyLock::new(|| default_path("mirrord.json"));

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
        // LayerConfig::resolve needs an existing file. Initialization deliberately avoids parsing
        // existing contents so regular config templating remains available on this path.
        initialize_empty_json_at_path(path).await?;

        let mut context = ConfigContext::default()
            .override_env(LayerConfig::FILE_PATH_ENV, path)
            .strict_env(true);
        let config = LayerConfig::resolve(&mut context).map_err(io::Error::other)?;

        Ok(Self { config })
    }

    /// Reads the sparse global configuration document without replacing or normalizing it.
    async fn read() -> Result<BTreeMap<String, Value>, GlobalConfigError> {
        Self::read_at_path(GLOBAL_CONFIG_PATH.as_path()).await
    }

    async fn read_at_path(path: &Path) -> Result<BTreeMap<String, Value>, GlobalConfigError> {
        match tokio::fs::read(path).await {
            Ok(contents) => Ok(serde_json::from_slice(&contents)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(error.into()),
        }
    }

    /// Atomically mutates a strictly parsed sparse global configuration document.
    async fn mutate<E>(
        update: impl FnOnce(&mut BTreeMap<String, Value>) -> Result<(), E> + Send + 'static,
    ) -> Result<BTreeMap<String, Value>, E>
    where
        E: From<io::Error> + Send + 'static,
    {
        Self::mutate_at_path(GLOBAL_CONFIG_PATH.as_path(), update).await
    }

    async fn mutate_at_path<E>(
        path: &Path,
        update: impl FnOnce(&mut BTreeMap<String, Value>) -> Result<(), E> + Send + 'static,
    ) -> Result<BTreeMap<String, Value>, E>
    where
        E: From<io::Error> + Send + 'static,
    {
        update_at_path_strict(path, update).await
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

/// Errors returned by `mirrord config`.
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

    /// A dotted field path cannot identify an object field.
    #[error("Invalid config field path `{path}`: {message}")]
    InvalidFieldPath {
        /// Field path supplied by the user.
        path: String,
        /// Reason the path cannot be used.
        message: String,
    },

    /// A field path cannot be applied to the current document.
    #[error("Cannot update global configuration at `{path}`: {message}")]
    Mutation {
        /// Dotted field path supplied by the user.
        path: String,
        /// Reason the mutation cannot be applied.
        message: String,
    },
}

#[derive(Debug)]
struct FieldPath {
    dotted: String,
    pointer: PointerBuf,
}

impl GlobalConfigError {
    fn invalid_field_path(path: &str, message: impl Into<String>) -> Self {
        Self::InvalidFieldPath {
            path: path.to_owned(),
            message: message.into(),
        }
    }

    fn mutation_error(path: &str, message: impl Into<String>) -> Self {
        Self::Mutation {
            path: path.to_owned(),
            message: message.into(),
        }
    }
}

/// Handles all `mirrord config` subcommands.
pub(crate) async fn global_config_command(args: GlobalConfigArgs) -> Result<(), GlobalConfigError> {
    match args.command {
        GlobalConfigCommand::Show => show().await,
        GlobalConfigCommand::Set(args) => set(args).await,
        GlobalConfigCommand::Unset(args) => unset(args).await,
    }
}

async fn show() -> Result<(), GlobalConfigError> {
    let config = GlobalConfig::read().await?;
    println!("{}", serde_json::to_string_pretty(&config)?);
    Ok(())
}

async fn set(args: SetGlobalConfigArgs) -> Result<(), GlobalConfigError> {
    let path = FieldPath::parse(args.path)?;
    let value = parse_value(args.value);

    GlobalConfig::mutate(move |config| path.apply_set(config, value)).await?;

    Ok(())
}

async fn unset(args: UnsetGlobalConfigArgs) -> Result<(), GlobalConfigError> {
    let path = FieldPath::parse(args.path)?;

    GlobalConfig::mutate(move |config| path.apply_unset(config)).await?;

    Ok(())
}

fn parse_value(raw_value: String) -> Value {
    match serde_json::from_str(&raw_value) {
        Ok(value) => value,
        Err(_) => Value::String(raw_value),
    }
}

impl FieldPath {
    fn parse(path: String) -> Result<Self, GlobalConfigError> {
        if path.split('.').any(str::is_empty) {
            return Err(GlobalConfigError::invalid_field_path(
                &path,
                "field path segments cannot be empty",
            ));
        }

        if let Some(segment) = path
            .split('.')
            .find(|segment| *segment == "-" || segment.parse::<usize>().is_ok())
        {
            return Err(GlobalConfigError::invalid_field_path(
                &path,
                format!("array index segment `{segment}` is not supported"),
            ));
        }

        let pointer = PointerBuf::from_tokens(path.split('.'));
        Ok(Self {
            dotted: path,
            pointer,
        })
    }

    fn set_pointer(&self, document: &mut Value, new_value: Value) -> Result<(), GlobalConfigError> {
        document
            .assign(&self.pointer, new_value)
            .map(|_| ())
            .map_err(|error| GlobalConfigError::mutation_error(&self.dotted, error.to_string()))
    }

    fn unset_pointer(&self, document: &mut Value) -> Result<(), GlobalConfigError> {
        document
            .delete(&self.pointer)
            .map(|_| ())
            .ok_or_else(|| GlobalConfigError::mutation_error(&self.dotted, "value does not exist"))
    }

    fn apply_set(
        self,
        config: &mut BTreeMap<String, Value>,
        value: Value,
    ) -> Result<(), GlobalConfigError> {
        let mut candidate = serde_json::to_value(&*config)?;
        self.set_pointer(&mut candidate, value)?;
        GlobalConfig::validate_value(candidate.clone())?;
        *config = serde_json::from_value(candidate)?;
        Ok(())
    }

    fn apply_unset(self, config: &mut BTreeMap<String, Value>) -> Result<(), GlobalConfigError> {
        let mut candidate = serde_json::to_value(&*config)?;
        self.unset_pointer(&mut candidate)?;
        GlobalConfig::validate_value(candidate.clone())?;
        *config = serde_json::from_value(candidate)?;
        Ok(())
    }
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

    fn apply_test_set(
        config: &BTreeMap<String, Value>,
        path: &str,
        raw_value: &str,
    ) -> Result<BTreeMap<String, Value>, GlobalConfigError> {
        let path = FieldPath::parse(path.to_owned())?;
        let value = parse_value(raw_value.to_owned());
        let mut updated = config.clone();
        path.apply_set(&mut updated, value)?;
        Ok(updated)
    }

    fn apply_test_unset(
        config: &BTreeMap<String, Value>,
        path: &str,
    ) -> Result<BTreeMap<String, Value>, GlobalConfigError> {
        let path = FieldPath::parse(path.to_owned())?;
        let mut updated = config.clone();
        path.apply_unset(&mut updated)?;
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
        assert_eq!(fs::read(path).await.unwrap(), b"{}\n");
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
    fn set_updates_nested_field_with_plain_string_value() {
        let config =
            apply_test_set(&BTreeMap::new(), "agent.image", "custom.image/latest").unwrap();

        assert_eq!(
            serde_json::to_value(config).unwrap(),
            serde_json::json!({"agent": {"image": "custom.image/latest"}})
        );
    }

    #[test]
    fn set_parses_valid_json_into_typed_values() {
        let config = apply_test_set(&BTreeMap::new(), "operator", "true").unwrap();
        let config = apply_test_set(&config, "agent.ttl", "42").unwrap();
        let config = apply_test_set(
            &config,
            "agent.image",
            r#"{"registry":"custom.image","tag":"latest"}"#,
        )
        .unwrap();

        assert_eq!(
            serde_json::to_value(config).unwrap(),
            serde_json::json!({
                "agent": {
                    "image": {"registry": "custom.image", "tag": "latest"},
                    "ttl": 42,
                },
                "operator": true,
            })
        );
    }

    #[test]
    fn set_accepts_quoted_json_to_force_a_string() {
        let config = apply_test_set(&BTreeMap::new(), "agent.image", r#""true""#).unwrap();

        assert_eq!(
            config.get("agent"),
            Some(&serde_json::json!({"image": "true"}))
        );
    }

    #[test]
    fn dotted_path_conversion_escapes_json_pointer_tokens() {
        let pointer = FieldPath::parse("agent/image.registry~name".to_owned()).unwrap();

        assert_eq!(pointer.pointer.as_str(), "/agent~1image/registry~0name");
    }

    #[test]
    fn invalid_dotted_field_paths_are_rejected() {
        for path in [
            "",
            ".agent",
            "agent.",
            "agent..image",
            "agent.0.image",
            "agent.-.image",
        ] {
            let error = FieldPath::parse(path.to_owned()).unwrap_err();

            assert!(
                matches!(error, GlobalConfigError::InvalidFieldPath { .. }),
                "unexpected error for `{path}`: {error}"
            );
        }
    }

    #[test]
    fn set_rejects_unknown_regular_config_field() {
        let error = apply_test_set(&BTreeMap::new(), "booga", "true").unwrap_err();

        assert!(error.to_string().contains("booga"));
    }

    #[test]
    fn set_rejects_value_with_wrong_type() {
        let error = apply_test_set(&BTreeMap::new(), "operator", "wedel").unwrap_err();

        assert!(error.to_string().contains("boolean"));
    }

    #[test]
    fn unset_removes_nested_value_without_persisting_resolved_defaults() {
        let configured =
            apply_test_set(&BTreeMap::new(), "agent.image", "custom.image/latest").unwrap();
        let configured = apply_test_set(&configured, "agent.ttl", "42").unwrap();
        let config = apply_test_unset(&configured, "agent.image").unwrap();

        assert_eq!(
            serde_json::to_value(config).unwrap(),
            serde_json::json!({"agent": {"ttl": 42}})
        );
    }

    #[test]
    fn mutation_errors_use_the_original_dotted_path() {
        let config = BTreeMap::from([(
            "feature".to_owned(),
            serde_json::json!({"network": {"incoming": {"ports": []}}}),
        )]);
        let set_error =
            apply_test_set(&config, "feature.network.incoming.ports.named~port", "80").unwrap_err();
        let unset_error =
            apply_test_unset(&BTreeMap::new(), "feature/network.incoming~mode").unwrap_err();

        assert!(
            set_error
                .to_string()
                .contains("feature.network.incoming.ports.named~port")
        );
        assert!(
            set_error
                .to_string()
                .contains("failed to parse as an array index")
        );
        assert!(
            unset_error
                .to_string()
                .contains("feature/network.incoming~mode")
        );
        assert!(unset_error.to_string().contains("does not exist"));
        assert!(!unset_error.to_string().contains("~1"));
    }

    #[tokio::test]
    async fn set_incoming_mode_expands_and_persists_scalar_shorthand() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("mirrord.json");
        fs::write(&path, br#"{"feature":{"network":{"incoming":"mirror"}}}"#)
            .await
            .unwrap();
        let field_path = FieldPath::parse("feature.network.incoming.mode".to_owned()).unwrap();
        let value = parse_value("steal".to_owned());

        GlobalConfig::mutate_at_path(&path, move |config| field_path.apply_set(config, value))
            .await
            .unwrap();

        let stored: Value = serde_json::from_slice(&fs::read(path).await.unwrap()).unwrap();
        assert_eq!(
            stored,
            serde_json::json!({
                "feature": {"network": {"incoming": {"mode": "steal"}}}
            })
        );
        let _: LayerFileConfig = serde_json::from_value(stored).unwrap();
    }

    #[tokio::test]
    async fn read_only_output_preserves_existing_formatting() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("mirrord.json");
        let original = br#"{
    "operator" : true,
    "telemetry": false
}
"#;
        fs::write(&path, original).await.unwrap();

        let config = GlobalConfig::read_at_path(&path).await.unwrap();

        assert_eq!(config.get("operator"), Some(&Value::Bool(true)));
        assert_eq!(fs::read(path).await.unwrap(), original);
    }

    #[tokio::test]
    async fn read_only_output_preserves_unparseable_contents() {
        for original in [
            br#"{"telemetry": "unfinished""#.as_slice(),
            br#"{"telemetry": {{ false }}}"#,
        ] {
            let directory = tempdir().unwrap();
            let path = directory.path().join("mirrord.json");
            fs::write(&path, original).await.unwrap();

            assert!(GlobalConfig::read_at_path(&path).await.is_err());
            assert_eq!(fs::read(path).await.unwrap(), original);
        }
    }

    #[tokio::test]
    async fn set_and_unset_preserve_unparseable_contents() {
        for original in [
            br#"{"telemetry": "unfinished""#.as_slice(),
            br#"{"telemetry": {{ false }}}"#,
        ] {
            for set in [true, false] {
                let directory = tempdir().unwrap();
                let path = directory.path().join("mirrord.json");
                fs::write(&path, original).await.unwrap();

                let result = if set {
                    let field_path = FieldPath::parse("operator".to_owned()).unwrap();
                    let value = parse_value("true".to_owned());
                    GlobalConfig::mutate_at_path(&path, move |config| {
                        field_path.apply_set(config, value)
                    })
                    .await
                } else {
                    let field_path = FieldPath::parse("telemetry".to_owned()).unwrap();
                    GlobalConfig::mutate_at_path(&path, move |config| {
                        field_path.apply_unset(config)
                    })
                    .await
                };

                assert!(result.is_err());
                assert_eq!(fs::read(path).await.unwrap(), original);
            }
        }
    }

    #[tokio::test]
    async fn rejected_edits_preserve_existing_bytes() {
        enum RejectedEdit {
            InvalidPath,
            UnknownField,
            WrongType,
            MissingUnset,
        }

        let original = br#"{
  "operator" : true,
  "agent": {}
}
"#;

        for edit in [
            RejectedEdit::InvalidPath,
            RejectedEdit::UnknownField,
            RejectedEdit::WrongType,
            RejectedEdit::MissingUnset,
        ] {
            let directory = tempdir().unwrap();
            let path = directory.path().join("mirrord.json");
            fs::write(&path, original).await.unwrap();

            let result = match edit {
                RejectedEdit::InvalidPath => {
                    FieldPath::parse("agent..image".to_owned()).map(|_| BTreeMap::new())
                }
                RejectedEdit::UnknownField => {
                    let field_path = FieldPath::parse("agent.booga".to_owned()).unwrap();
                    let value = parse_value("true".to_owned());
                    GlobalConfig::mutate_at_path(&path, move |config| {
                        field_path.apply_set(config, value)
                    })
                    .await
                }
                RejectedEdit::WrongType => {
                    let field_path = FieldPath::parse("operator".to_owned()).unwrap();
                    let value = parse_value("wedel".to_owned());
                    GlobalConfig::mutate_at_path(&path, move |config| {
                        field_path.apply_set(config, value)
                    })
                    .await
                }
                RejectedEdit::MissingUnset => {
                    let field_path = FieldPath::parse("telemetry".to_owned()).unwrap();
                    GlobalConfig::mutate_at_path(&path, move |config| {
                        field_path.apply_unset(config)
                    })
                    .await
                }
            };

            assert!(result.is_err());
            assert_eq!(fs::read(path).await.unwrap(), original);
        }
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

        assert_eq!(
            fs::read(path).await.unwrap(),
            b"{\n  \"operator\": true\n}\n"
        );
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
