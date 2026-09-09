use std::{
    io,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use mirrord_config::{LayerConfig, LayerFileConfig, config::ConfigContext};

use super::{default_path, initialize_empty_json_at_path, update_at_path_strict};

/// "~/.mirrord/mirrord.json"
static GLOBAL_CONFIG_PATH: LazyLock<PathBuf> = LazyLock::new(|| default_path("mirrord.json"));

/// A regular mirrord configuration loaded from the user-wide config path.
#[derive(Debug)]
pub(crate) struct GlobalConfig {
    config: LayerConfig,
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

    /// Records that an operator session succeeded.
    pub(crate) async fn remember_operator() -> io::Result<()> {
        Self::remember_operator_at_path(GLOBAL_CONFIG_PATH.as_path()).await
    }

    async fn remember_operator_at_path(path: &Path) -> io::Result<()> {
        update_at_path_strict(path, |config: &mut LayerFileConfig| {
            config.operator = Some(true);
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
