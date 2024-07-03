use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Target, TargetDisplay};

/// <!--${internal}-->
/// Transparent wrapper around a [`Target`]!
///
/// Only intelligent people may see this type.
///
/// If you have to write `.0` anywhere when dealing with this, add whatever missing trait
/// impl (like `DerefMut`), instead of actually using `.0`, otherwise the emperor might
/// think you're not intelligent.
///
/// ## `serde` implementation
///
/// [`Serialize`] and [`Deserialize`] are _manually-ish_ implemented to keep this
/// type as transparent as possible, and to handle the [`Target::Unknown`] variant.
///
/// [`Deserialize`] happens in two steps:
/// 1. deserialize the type as a [`serde_json::Value`], where an error here means an
/// an actual deserialization issue;
/// 2. convert the [`serde_json::Value`] into a [`Target`], turning an error into
/// [`Target::Unknown`].
#[derive(Clone, Eq, PartialEq, Hash, Debug, JsonSchema)]
#[serde(transparent, deny_unknown_fields)]
pub struct TargetHandle(pub Target);

#[derive(Serialize, Clone, Debug, JsonSchema)]
#[serde(untagged)]
pub enum FutureProofTarget {
    #[serde(serialize_with = "Target::serialize")]
    Known(Target),
    #[serde(skip_serializing)]
    Unknown(String),
}

impl FutureProofTarget {
    pub fn known(&self) -> Option<&Target> {
        match self {
            FutureProofTarget::Known(target) => Some(target),
            FutureProofTarget::Unknown(_) => None,
        }
    }
}

impl From<Target> for FutureProofTarget {
    fn from(target: Target) -> Self {
        Self::Known(target)
    }
}

impl core::fmt::Display for FutureProofTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FutureProofTarget::Known(target) => target.fmt(f),
            FutureProofTarget::Unknown(unknown) => write!(f, "{}", unknown),
        }
    }
}

impl TargetDisplay for FutureProofTarget {
    fn type_(&self) -> &str {
        match self {
            FutureProofTarget::Known(target) => target.type_(),
            FutureProofTarget::Unknown(_) => unreachable!("Unknown target has no type!"),
        }
    }

    fn name(&self) -> &str {
        match self {
            FutureProofTarget::Known(target) => target.name(),
            FutureProofTarget::Unknown(_) => unreachable!(),
        }
    }

    fn container(&self) -> Option<&String> {
        match self {
            FutureProofTarget::Known(target) => target.container(),
            FutureProofTarget::Unknown(_) => unreachable!("Unknown target has no name!"),
        }
    }
}

impl<'de> Deserialize<'de> for FutureProofTarget {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let deserialized = serde_json::Value::deserialize(deserializer)?;
        let maybe_unknown = deserialized.to_string();

        let target = serde_json::from_value::<Target>(deserialized);
        match target {
            Ok(target) => Ok(FutureProofTarget::Known(target)),
            Err(_) => Ok(FutureProofTarget::Unknown(maybe_unknown)),
        }
    }
}
