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

impl core::fmt::Display for TargetHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl TargetDisplay for TargetHandle {
    fn type_(&self) -> &str {
        self.0.type_()
    }

    fn name(&self) -> &str {
        self.0.name()
    }

    fn container(&self) -> Option<&String> {
        self.0.container()
    }
}

impl core::ops::Deref for TargetHandle {
    type Target = Target;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl core::borrow::Borrow<Target> for TargetHandle {
    fn borrow(&self) -> &Target {
        self.0.borrow()
    }
}

impl From<Target> for TargetHandle {
    fn from(value: Target) -> Self {
        Self(value)
    }
}

impl Serialize for TargetHandle {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TargetHandle {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let deserialized = serde_json::Value::deserialize(deserializer)?;
        let target_name = deserialized.get("name").map(ToString::to_string);

        let target = serde_json::from_value(deserialized);
        match target {
            Ok(target) => Ok(TargetHandle(target)),
            Err(_) => Ok(TargetHandle(Target::Unknown(
                target_name.unwrap_or_default(),
            ))),
        }
    }
}
