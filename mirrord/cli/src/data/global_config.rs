use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use mirrord_config::{LayerConfig, config::ConfigContext};
use serde_json::Value;

use super::{default_path, update_at_path};

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
        // LayerConfig::resolve needs an existing file, so use the shared locked, atomic update
        // path to initialize it and recover invalid JSON as an empty object. A raw map preserves
        // arbitrary fields without persisting resolved LayerConfig defaults.
        let _: BTreeMap<String, Value> = update_at_path(path, |_| {}).await?;

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
        update_at_path(path, |config: &mut BTreeMap<String, Value>| {
            config.insert("operator".to_owned(), Value::Bool(true));
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
    };
    use tempfile::tempdir;
    use tokio::fs;

    use super::*;

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
