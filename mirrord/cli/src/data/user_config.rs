use std::{
    collections::BTreeMap,
    io,
    ops::Not,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use jsonptr::{Assign, Delete, Pointer, PointerBuf};
use miette::Diagnostic;
use mirrord_config::LayerConfig;
use mirrord_kube::api::kubernetes::resolve_kube_context;
use serde::{Deserialize, Serialize, de::IntoDeserializer};
use serde_json::Value;
use thiserror::Error;
use tracing::trace;

use super::{default_path, update_at_path};
use crate::config::user_config::{
    SetUserConfigArgs, UnsetUserConfigArgs, UserConfigArgs, UserConfigCommand,
};

/// "~/.mirrord/user.json"
static USER_STORE_PATH: LazyLock<PathBuf> = LazyLock::new(|| default_path("user.json"));

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
    ///
    /// This preference is learned only after an operator session starts successfully. We do not
    /// clear it after later discovery, authentication, license, or connection failures because
    /// those failures can be transient; clearing it would allow the next run to silently use OSS.
    /// Users can still choose OSS explicitly with `operator = false`.
    #[serde(default)]
    operator: bool,
}

/// Invalid portable user configuration supplied through a strict input boundary.
#[derive(Debug, Error)]
pub(crate) enum UserConfigValidationError {
    /// The input does not match the serialized shape of [`UserConfig`].
    #[error("Invalid user-wide configuration: {0}")]
    Json(#[from] serde_json::Error),

    /// Persistent data remains forward-compatible, but user input must not silently ignore typos.
    #[error("Unknown user-wide configuration field `{0}`")]
    UnknownField(String),

    /// A complete configuration is always represented by a JSON object.
    #[error("expected a user-wide configuration JSON object")]
    ExpectedObject,
}

impl UserConfig {
    /// Creates `UserConfig` from the default file path (`USER_STORE_PATH`).
    pub(crate) async fn from_default_path() -> io::Result<Self> {
        Self::from_path(USER_STORE_PATH.as_path()).await
    }

    async fn from_path(path: &Path) -> io::Result<Self> {
        update_at_path(path, |_| Ok(())).await
    }

    /// Atomically updates the user-wide configuration stored at the default path.
    pub(crate) async fn update<E>(
        update: impl FnOnce(&mut Self) -> Result<(), E> + Send + 'static,
    ) -> Result<Self, E>
    where
        E: From<io::Error> + Send + 'static,
    {
        update_at_path(USER_STORE_PATH.as_path(), update).await
    }

    /// Records that an operator session succeeded in the given Kubernetes context.
    pub(crate) async fn remember_operator(context: String) -> io::Result<()> {
        Self::update(move |config| {
            config.set_operator(context);
            Ok::<_, io::Error>(())
        })
        .await?;
        Ok(())
    }

    /// Deserializes a complete configuration while rejecting fields ignored by regular Serde
    /// deserialization.
    pub(crate) fn from_strict_value(value: Value) -> Result<Self, UserConfigValidationError> {
        if value.is_object().not() {
            return Err(UserConfigValidationError::ExpectedObject);
        }

        let mut unknown_field = None;
        let config = serde_ignored::deserialize(value.into_deserializer(), |path| {
            unknown_field.get_or_insert_with(|| path.to_string());
        })?;

        if let Some(path) = unknown_field {
            return Err(UserConfigValidationError::UnknownField(path));
        }

        Ok(config)
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

/// Errors returned by `mirrord user-config`.
#[derive(Debug, Diagnostic, Error)]
pub(crate) enum UserConfigError {
    /// Reading or updating `~/.mirrord/user.json` failed.
    #[error("Failed accessing user-wide mirrord configuration: {0}")]
    Io(#[from] io::Error),

    /// Serializing the current configuration failed.
    #[error("Failed processing user-wide mirrord configuration JSON: {0}")]
    Json(#[from] serde_json::Error),

    /// The resulting document does not conform to [`UserConfig`].
    #[error(transparent)]
    Validation(#[from] UserConfigValidationError),

    /// A set argument omitted its assignment separator.
    #[error("Invalid user-wide configuration assignment `{0}`; expected `/json/pointer=value`")]
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
    #[error("Cannot update user-wide configuration at `{pointer}`: {message}")]
    Mutation {
        /// Pointer supplied by the user.
        pointer: String,
        /// Reason the mutation cannot be applied.
        message: String,
    },
}

impl UserConfigError {
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

/// Handles all `mirrord user-config` subcommands.
pub(crate) async fn user_config_command(args: UserConfigArgs) -> Result<(), UserConfigError> {
    match args.command {
        UserConfigCommand::Show => show().await,
        UserConfigCommand::Set(args) => set(args).await,
        UserConfigCommand::Unset(args) => unset(args).await,
    }
}

async fn show() -> Result<(), UserConfigError> {
    let config = UserConfig::from_default_path().await?;
    println!("{}", serde_json::to_string_pretty(&config)?);
    Ok(())
}

async fn set(args: SetUserConfigArgs) -> Result<(), UserConfigError> {
    let assignments = args
        .assignments
        .into_iter()
        .map(parse_assignment)
        .collect::<Result<Vec<_>, _>>()?;

    UserConfig::update(|config| {
        let mut candidate = serde_json::to_value(&*config)?;
        for assignment in assignments {
            set_pointer(&mut candidate, &assignment.pointer, assignment.value)?;
        }
        *config = UserConfig::from_strict_value(candidate)?;
        Ok::<_, UserConfigError>(())
    })
    .await?;

    Ok(())
}

async fn unset(args: UnsetUserConfigArgs) -> Result<(), UserConfigError> {
    let pointers = args
        .pointers
        .into_iter()
        .map(parse_pointer)
        .collect::<Result<Vec<_>, _>>()?;

    UserConfig::update(|config| {
        let mut candidate = serde_json::to_value(&*config)?;
        for pointer in pointers {
            unset_pointer(&mut candidate, &pointer)?;
        }
        *config = UserConfig::from_strict_value(candidate)?;
        Ok::<_, UserConfigError>(())
    })
    .await?;

    Ok(())
}

fn parse_assignment(assignment: String) -> Result<Assignment, UserConfigError> {
    let Some((pointer, raw_value)) = assignment.split_once('=') else {
        return Err(UserConfigError::InvalidAssignment(assignment));
    };
    let pointer = parse_pointer(pointer.to_owned())?;

    let value =
        serde_json::from_str(raw_value).unwrap_or_else(|_| Value::String(raw_value.to_owned()));

    Ok(Assignment { pointer, value })
}

fn parse_pointer(pointer: String) -> Result<PointerBuf, UserConfigError> {
    let parsed = PointerBuf::parse(pointer)
        .map_err(|error| UserConfigError::invalid_pointer(error.subject(), error.to_string()))?;
    if parsed.is_root() {
        return Err(UserConfigError::invalid_pointer(
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
) -> Result<(), UserConfigError> {
    document
        .assign(pointer, new_value)
        .map(|_| ())
        .map_err(|error| UserConfigError::mutation_error(pointer.as_str(), error.to_string()))
}

fn unset_pointer(document: &mut Value, pointer: &Pointer) -> Result<(), UserConfigError> {
    document
        .delete(pointer)
        .map(|_| ())
        .ok_or_else(|| UserConfigError::mutation_error(pointer.as_str(), "value does not exist"))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;
    use tokio::fs;

    use mirrord_config::{
        LayerFileConfig,
        config::{ConfigContext, MirrordConfig},
    };

    use super::*;

    fn apply_set(config: &UserConfig, assignments: &[&str]) -> Result<UserConfig, UserConfigError> {
        let mut candidate = serde_json::to_value(config)?;
        for assignment in assignments {
            let assignment = parse_assignment((*assignment).to_owned())?;
            set_pointer(&mut candidate, &assignment.pointer, assignment.value)?;
        }
        Ok(UserConfig::from_strict_value(candidate)?)
    }

    fn apply_unset(config: &UserConfig, pointers: &[&str]) -> Result<UserConfig, UserConfigError> {
        let mut candidate = serde_json::to_value(config)?;
        for pointer in pointers {
            let pointer = parse_pointer((*pointer).to_owned())?;
            unset_pointer(&mut candidate, &pointer)?;
        }
        Ok(UserConfig::from_strict_value(candidate)?)
    }

    #[tokio::test]
    async fn user_config_is_stored_as_its_own_document() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("user.json");

        let updated = update_at_path(&path, |config: &mut UserConfig| {
            config.set_operator("wawel".to_owned());
            Ok::<_, io::Error>(())
        })
        .await
        .unwrap();

        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(path).await.unwrap()).unwrap(),
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

    #[test]
    fn set_updates_multiple_values_without_field_specific_handling() {
        let config = apply_set(
            &UserConfig::default(),
            &["/kube_context=wawel", "/contexts/wawel/operator=true"],
        )
        .unwrap();

        assert_eq!(
            serde_json::to_value(config).unwrap(),
            serde_json::json!({
                "kube_context": "wawel",
                "contexts": {"wawel": {"operator": true}}
            })
        );
    }

    #[test]
    fn set_decodes_json_pointer_escapes() {
        let config = apply_set(
            &UserConfig::default(),
            &["/contexts/wawel~1malbork/operator=true"],
        )
        .unwrap();

        assert_eq!(
            serde_json::to_value(config)
                .unwrap()
                .pointer("/contexts/wawel~1malbork/operator"),
            Some(&Value::Bool(true))
        );
    }

    #[test]
    fn set_preserves_equals_signs_in_plain_string_values() {
        let config = apply_set(&UserConfig::default(), &["/kube_context=wawel=malbork"]).unwrap();

        assert_eq!(
            serde_json::to_value(config)
                .unwrap()
                .pointer("/kube_context"),
            Some(&Value::String("wawel=malbork".to_owned()))
        );
    }

    #[test]
    fn set_accepts_quoted_string_that_looks_like_boolean() {
        let assignment = parse_assignment(r#"/kube_context="true""#.to_owned()).unwrap();

        assert_eq!(assignment.value, Value::String("true".to_owned()));
    }

    #[test]
    fn set_rejects_unknown_field() {
        let error = apply_set(&UserConfig::default(), &["/booga=true"]).unwrap_err();

        assert!(error.to_string().contains("booga"));
    }

    #[test]
    fn set_rejects_value_with_wrong_type() {
        let error = apply_set(
            &UserConfig::default(),
            &["/contexts/malbork/operator=wedel"],
        )
        .unwrap_err();

        assert!(error.to_string().contains("boolean"));
    }

    #[test]
    fn unset_removes_top_level_and_nested_values() {
        let configured = apply_set(
            &UserConfig::default(),
            &["/kube_context=wawel", "/contexts/malbork/operator=true"],
        )
        .unwrap();
        let config = apply_unset(
            &configured,
            &["/kube_context", "/contexts/malbork/operator"],
        )
        .unwrap();

        assert_eq!(
            serde_json::to_value(config).unwrap(),
            serde_json::json!({
                "kube_context": null,
                "contexts": {"malbork": {"operator": false}}
            })
        );
    }

    #[test]
    fn unset_rejects_missing_value() {
        let error = apply_unset(&UserConfig::default(), &["/contexts/wawel"]).unwrap_err();

        assert!(error.to_string().contains("does not exist"));
    }

    #[test]
    fn mutation_rejects_malformed_json_pointer() {
        let error =
            apply_set(&UserConfig::default(), &["contexts/wawel/operator=true"]).unwrap_err();

        assert!(error.to_string().contains("does not start with a slash"));
    }
}
