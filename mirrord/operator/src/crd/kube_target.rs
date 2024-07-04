use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Target;

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
/// [`Target::Unknown`].
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
