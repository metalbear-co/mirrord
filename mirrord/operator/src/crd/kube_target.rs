use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Target;
use crate::client::error::OperatorApiError;

/// Holds either a kubernetes target that we know about, (de)serializing it into a
/// [`Target`], or a target we do not know about.
///
/// It's main purpose is to provide forward compatibility with targets between the operator
/// and mirrord, so when we add new targets over there, they can reported back through
/// `mirrord ls` (or other ways of listing targets).
///
/// You should avoid passing this type around, instead try to get the `Known` variant
/// out, and potentially throw an error if it's an `Unknown` target. If you feel compelled
/// to write methods for this type, think again, you probaly don't want to do that.
///
/// ## Why not an `Option`
///
/// Due to how we used to treat a `None` `Option<Target>` as meaning [`Target::Targetless`],
/// we can't just change it to `None` meaning _unknown_, so this type is basically acting
/// as a custom `Option<Target>` for this purpose.
///
/// ## `serde` implementation
///
/// [`Deserialize`] is _manually-ish_ implemented to handle the `Unknown` variant.
///
/// [`Deserialize`] happens in two steps:
/// 1. deserialize the type as a [`serde_json::Value`], where an error here means an
/// an actual deserialization issue;
/// 2. convert the [`serde_json::Value`] into a [`Target`], turning an error into
/// [`KubeTarget::Unknown`].
#[derive(Serialize, Clone, Debug, JsonSchema)]
#[serde(untagged)]
pub enum KubeTarget {
    /// A target that we know of in both mirrord and the operator.
    ///
    /// Avoid `match`ing on this, you should be using [`KubeTarget::known`] instead.
    #[serde(serialize_with = "Target::serialize")]
    Known(Target),

    /// A target that has been added in the operator, but the current version of mirrord
    /// doesn't know about.
    ///
    /// Should be ignored in most cases.
    #[serde(skip_serializing)]
    Unknown(String),
}

impl KubeTarget {
    /// Instead of `match`ing on [`KubeTarget`], you should use this method.
    pub fn known(&self) -> Option<&Target> {
        match self {
            KubeTarget::Known(target) => Some(target),
            KubeTarget::Unknown(_) => None,
        }
    }
}

impl TryFrom<KubeTarget> for Target {
    type Error = OperatorApiError;

    fn try_from(kube_target: KubeTarget) -> Result<Self, Self::Error> {
        match kube_target {
            KubeTarget::Known(target) => Ok(target),
            KubeTarget::Unknown(unknown) => Err(OperatorApiError::UnknownTarget(unknown)),
        }
    }
}

impl AsRef<KubeTarget> for KubeTarget {
    fn as_ref(&self) -> &KubeTarget {
        self
    }
}

impl<'a> TryFrom<&'a KubeTarget> for &'a Target {
    type Error = OperatorApiError;

    fn try_from(kube_target: &'a KubeTarget) -> Result<Self, Self::Error> {
        match kube_target {
            KubeTarget::Known(target) => Ok(target),
            KubeTarget::Unknown(unknown) => {
                Err(OperatorApiError::UnknownTarget(unknown.to_owned()))
            }
        }
    }
}

impl From<Target> for KubeTarget {
    fn from(target: Target) -> Self {
        Self::Known(target)
    }
}

impl core::fmt::Display for KubeTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KubeTarget::Known(target) => target.fmt(f),
            KubeTarget::Unknown(unknown) => write!(f, "{}", unknown),
        }
    }
}

impl<'de> Deserialize<'de> for KubeTarget {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let deserialized = serde_json::Value::deserialize(deserializer)?;
        let maybe_unknown = deserialized.to_string();

        let target = serde_json::from_value::<Target>(deserialized);
        match target {
            Ok(target) => Ok(KubeTarget::Known(target)),
            Err(_) => Ok(KubeTarget::Unknown(maybe_unknown)),
        }
    }
}

#[cfg(test)]
mod test {
    use kube::CustomResource;
    use mirrord_config::target::Target;
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    use crate::crd::{kube_target::KubeTarget, TargetSpec};

    #[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
    #[kube(
        group = "operator.metalbear.co",
        version = "v1",
        kind = "Target",
        root = "LegacyTargetCrd",
        namespaced
    )]
    struct LegacyTargetSpec {
        target: Option<Target>,
    }

    #[test]
    fn none_into_kube_target() {
        let legacy = serde_json::to_string_pretty(&LegacyTargetSpec { target: None }).unwrap();
        serde_json::from_str::<TargetSpec>(&legacy).expect("Deserialization from old to new!");
    }

    #[test]
    fn some_into_kube_target() {
        let legacy = serde_json::to_string_pretty(&LegacyTargetSpec {
            target: Some(Target::Targetless),
        })
        .unwrap();
        serde_json::from_str::<TargetSpec>(&legacy).expect("Deserialization from old to new!");
    }

    #[test]
    fn kube_target_unknown() {
        let new = serde_json::from_str::<TargetSpec>(&r#"{"Bolesław": "the Great"}"#)
            .expect("Deserialization of unknown!");

        assert!(matches!(
            new,
            TargetSpec {
                target: KubeTarget::Unknown(_)
            }
        ))
    }

    #[test]
    fn kube_target_to_legacy() {
        let new = serde_json::to_string_pretty(&TargetSpec {
            target: KubeTarget::Known(Target::Targetless),
        })
        .unwrap();

        serde_json::from_str::<LegacyTargetSpec>(&new).expect("Deserialization from new to old!");
    }
}
