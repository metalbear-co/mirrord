use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use mirrord_config::LayerConfig;
use mirrord_kube::api::kubernetes::resolve_kube_context;
use serde::{Deserialize, Serialize};
use tracing::trace;

use crate::mirrord_data;

/// "~/.mirrord/user.json"
static USER_STORE_PATH: LazyLock<PathBuf> =
    LazyLock::new(|| mirrord_data::default_path("user.json"));

/// User-wide mirrord defaults that are applied after project and environment configuration.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct UserConfig {
    /// Kubernetes context used when a mirrord invocation does not choose one explicitly.
    ///
    /// Unlike operator detection, mirrord never selects this default automatically.
    #[serde(default)]
    kube_context: Option<String>,

    /// Settings that are safe to reuse only when the same Kubernetes context is selected.
    ///
    /// When the user starts a session, mirrord remembers the (kube) context and applies these
    /// settings automatically.
    #[serde(default)]
    contexts: BTreeMap<String, ContextUserConfig>,
}

/// User-wide settings tied to a Kubernetes context.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
struct ContextUserConfig {
    /// Prevents an operator-capable context from silently falling back to OSS mode.
    #[serde(default)]
    operator: bool,
}

impl UserConfig {
    /// Creates `UserConfig` from the default file path (`USER_STORE_PATH`).
    pub(crate) async fn from_default_path() -> io::Result<Self> {
        Self::from_path(USER_STORE_PATH.as_path()).await
    }

    async fn from_path(path: &Path) -> io::Result<Self> {
        mirrord_data::update_at_path(path, |_| {}).await
    }

    /// Records that an operator session succeeded in the given Kubernetes context.
    pub(crate) async fn remember_operator(context: String) -> io::Result<()> {
        mirrord_data::update_at_path(USER_STORE_PATH.as_path(), move |config: &mut Self| {
            config.set_operator(context)
        })
        .await?;
        Ok(())
    }

    /// Applies user-wide defaults without overriding values selected by project config, CLI flags,
    /// or environment variables.
    pub(crate) fn apply_to(&self, config: &mut LayerConfig) {
        if config.kube_context.is_none() {
            config.kube_context.clone_from(&self.kube_context);
        }

        if config.operator.is_some() {
            return;
        }

        let Some(context) = effective_kube_context(config) else {
            return;
        };

        if self
            .contexts
            .get(&context)
            .is_some_and(|settings| settings.operator)
        {
            config.operator = Some(true);
        }
    }

    pub(crate) fn set_operator(&mut self, context: String) {
        self.contexts.entry(context).or_default().operator = true;
    }
}

/// Determines the context whose settings can safely be applied to this resolved config.
pub(crate) fn effective_kube_context(config: &LayerConfig) -> Option<String> {
    resolve_kube_context(config.kubeconfig.as_deref(), config.kube_context.as_deref())
        .inspect_err(|error| trace!(%error, "Failed resolving effective Kubernetes context"))
        .ok()
        .flatten()
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
    async fn user_config_is_stored_as_its_own_document() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("user.json");

        let updated = mirrord_data::update_at_path(&path, |config: &mut UserConfig| {
            config.set_operator("wawel".to_owned());
        })
        .await
        .unwrap();

        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&fs::read(path).await.unwrap()).unwrap(),
            serde_json::json!({
                "kube_context": null,
                "contexts": {"wawel": {"operator": true}}
            })
        );
        assert_eq!(updated.contexts.len(), 1);
    }

    #[test]
    fn user_config_applies_defaults_to_unset_layer_config() {
        let mut context = ConfigContext::default().strict_env(true);
        let mut layer_config = LayerFileConfig::default()
            .generate_config(&mut context)
            .unwrap();
        let mut user_config = UserConfig {
            kube_context: Some("wawel".to_owned()),
            ..Default::default()
        };
        user_config.set_operator("wawel".to_owned());

        user_config.apply_to(&mut layer_config);

        assert_eq!(
            (layer_config.kube_context, layer_config.operator),
            (Some("wawel".to_owned()), Some(true))
        );
    }

    #[test]
    fn explicit_oss_mode_overrides_remembered_operator() {
        let mut context = ConfigContext::default().strict_env(true);
        let mut layer_config = LayerFileConfig {
            operator: Some(false),
            ..Default::default()
        }
        .generate_config(&mut context)
        .unwrap();
        let mut user_config = UserConfig {
            kube_context: Some("wawel".to_owned()),
            ..Default::default()
        };
        user_config.set_operator("wawel".to_owned());

        user_config.apply_to(&mut layer_config);

        assert_eq!(layer_config.operator, Some(false));
    }

    #[test]
    fn remembered_operator_does_not_apply_to_another_context() {
        let mut context = ConfigContext::default()
            .override_env("MIRRORD_KUBE_CONTEXT", "malbork")
            .strict_env(true);
        let mut layer_config = LayerFileConfig::default()
            .generate_config(&mut context)
            .unwrap();
        let mut user_config = UserConfig::default();
        user_config.set_operator("wawel".to_owned());

        user_config.apply_to(&mut layer_config);

        assert_eq!(layer_config.operator, None);
    }

    #[test]
    fn user_config_serialization_is_deterministic() {
        let mut user_config = UserConfig {
            kube_context: Some("wawel".to_owned()),
            ..Default::default()
        };
        user_config.set_operator("malbork".to_owned());
        user_config.set_operator("wawel".to_owned());

        let serialized = serde_json::to_string_pretty(&user_config).unwrap();

        assert_eq!(
            serialized,
            r#"{
  "kube_context": "wawel",
  "contexts": {
    "malbork": {
      "operator": true
    },
    "wawel": {
      "operator": true
    }
  }
}"#
        );
    }
}
