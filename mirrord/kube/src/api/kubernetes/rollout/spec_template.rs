use k8s_openapi::api::core::v1::PodTemplateSpec;
use serde::{de, Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct RolloutSpecTemplate(#[serde(deserialize_with = "rollout_pod_spec")] PodTemplateSpec);

impl AsRef<PodTemplateSpec> for RolloutSpecTemplate {
    fn as_ref(&self) -> &PodTemplateSpec {
        &self.0
    }
}

/// Custom deserializer for a rollout template field due to
/// [#548](https://github.com/metalbear-co/operator/issues/548)
/// First deserializes it as value, fixes possible issues and then deserializes it as
/// PodTemplateSpec.
#[tracing::instrument(level = "debug", skip(deserializer), ret, err)]
pub fn rollout_pod_spec<'de, D>(deserializer: D) -> Result<PodTemplateSpec, D::Error>
where
    D: de::Deserializer<'de>,
{
    let mut value = serde_json::Value::deserialize(deserializer)?;

    value
        .get_mut("spec")
        .and_then(|spec| spec.get_mut("containers")?.as_array_mut())
        .into_iter()
        .flatten()
        .filter_map(|container| container.get_mut("resources"))
        .for_each(|resources| {
            for field in ["limits", "requests"] {
                let Some(object) = resources.get_mut(field) else {
                    continue;
                };

                for field in ["cpu", "memory"] {
                    let Some(raw) = object.get_mut(field) else {
                        continue;
                    };

                    if let Some(number) = raw.as_number() {
                        *raw = number.to_string().into();
                    }
                }
            }
        });

    serde_json::from_value(value).map_err(de::Error::custom)
}
