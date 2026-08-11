use std::{borrow::Cow, fmt, ops::Not};

use k8s_openapi::{
    Resource,
    api::{
        apps::v1::{Deployment, ReplicaSet, StatefulSet},
        batch::v1::{CronJob, Job},
        core::v1::{Pod, Service},
    },
    apimachinery::pkg::apis::meta::v1::LabelSelector,
};
use kube::core::Selector;
use mirrord_config::target::{Target, label::LabelTarget};
use mirrord_kube::api::kubernetes::rollout::Rollout;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Describes an owner of a mirrord session.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionOwner {
    /// Unique ID.
    pub user_id: String,
    /// Name of the POSIX user that executed the CLI command.
    pub username: String,
    /// Hostname of the machine where the CLI command was executed.
    pub hostname: String,
    /// Name of the Kubernetes user who's identity was assumed by the CLI.
    pub k8s_username: String,
}

impl fmt::Display for SessionOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}/{}@{}",
            self.username, self.k8s_username, self.hostname,
        )
    }
}

/// Describes a target of a mirrord session.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum SessionTarget {
    KubeResource(KubeResourceTarget),
    PodSet(PodSetTarget),
}

/// A single Kubernetes resource targeted by a mirrord session.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KubeResourceTarget {
    /// Kubernetes resource apiVersion.
    pub api_version: String,
    /// Kubernetes resource kind.
    pub kind: String,
    /// Target name.
    pub name: String,
    /// Name of the container defined in the Pod spec.
    pub container: String,
}

/// A set of pods targeted by a mirrord session.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PodSetTarget {
    /// Label selector for the targeted pods.
    #[schemars(schema_with = "label_selector_serde::schema")]
    #[serde(with = "label_selector_serde")]
    label_selector: Selector,

    /// Name of the container defined in the Pod spec.
    pub container: String,
}

impl PodSetTarget {
    pub fn new(label_selector: Selector, container: String) -> Self {
        Self {
            label_selector: Self::normalize(label_selector),
            container,
        }
    }

    pub fn label_selector(&self) -> &Selector {
        &self.label_selector
    }

    fn normalize(selector: Selector) -> Selector {
        let mut expressions = selector.into_iter().collect::<Vec<_>>();
        expressions.sort_unstable();
        expressions.into_iter().collect()
    }
}

impl fmt::Display for KubeResourceTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.kind, self.name)?;
        if !self.container.is_empty() {
            write!(f, "/container/{}", self.container)?;
        }
        Ok(())
    }
}

impl KubeResourceTarget {
    pub fn into_config(self) -> Option<Target> {
        let path = if self.container.is_empty() {
            format!("{}/{}", self.kind.to_ascii_lowercase(), self.name)
        } else {
            format!(
                "{}/{}/container/{}",
                self.kind.to_ascii_lowercase(),
                self.name,
                self.container
            )
        };

        path.parse().ok()
    }
}

impl fmt::Display for PodSetTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label_selector)?;
        if !self.container.is_empty() {
            write!(f, "/container/{}", self.container)?;
        }
        Ok(())
    }
}

impl fmt::Display for SessionTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KubeResource(target) => target.fmt(f),
            Self::PodSet(target) => target.fmt(f),
        }
    }
}

impl SessionTarget {
    pub fn target_type(&self) -> &str {
        match self {
            Self::KubeResource(target) => &target.kind,
            Self::PodSet(_) => "PodSet",
        }
    }

    pub fn display_name(&self) -> Cow<'_, str> {
        match self {
            Self::KubeResource(target) => Cow::Borrowed(&target.name),
            Self::PodSet(target) => Cow::Owned(target.label_selector.to_string()),
        }
    }

    pub fn container(&self) -> &str {
        match self {
            Self::KubeResource(target) => &target.container,
            Self::PodSet(target) => &target.container,
        }
    }

    /// Create a [`SessionTarget`] from a [`Target`] with a resolved container.
    ///
    /// Returns `None` for [`Target::Targetless`] or if the [`Target`] doesn't have a container.
    pub fn from_config(target: Target) -> Option<Self> {
        match target {
            Target::Deployment(t) => Some(Self::KubeResource(KubeResourceTarget {
                api_version: <Deployment as Resource>::API_VERSION.to_owned(),
                kind: <Deployment as Resource>::KIND.to_owned(),
                name: t.deployment,
                container: t.container?,
            })),
            Target::Pod(t) => Some(Self::KubeResource(KubeResourceTarget {
                api_version: <Pod as Resource>::API_VERSION.to_owned(),
                kind: <Pod as Resource>::KIND.to_owned(),
                name: t.pod,
                container: t.container?,
            })),
            Target::Rollout(t) => Some(Self::KubeResource(KubeResourceTarget {
                api_version: <Rollout as Resource>::API_VERSION.to_owned(),
                kind: <Rollout as Resource>::KIND.to_owned(),
                name: t.rollout,
                container: t.container?,
            })),
            Target::Job(t) => Some(Self::KubeResource(KubeResourceTarget {
                api_version: <Job as Resource>::API_VERSION.to_owned(),
                kind: <Job as Resource>::KIND.to_owned(),
                name: t.job,
                container: t.container?,
            })),
            Target::CronJob(t) => Some(Self::KubeResource(KubeResourceTarget {
                api_version: <CronJob as Resource>::API_VERSION.to_owned(),
                kind: <CronJob as Resource>::KIND.to_owned(),
                name: t.cron_job,
                container: t.container?,
            })),
            Target::StatefulSet(t) => Some(Self::KubeResource(KubeResourceTarget {
                api_version: <StatefulSet as Resource>::API_VERSION.to_owned(),
                kind: <StatefulSet as Resource>::KIND.to_owned(),
                name: t.stateful_set,
                container: t.container?,
            })),
            Target::Service(t) => Some(Self::KubeResource(KubeResourceTarget {
                api_version: <Service as Resource>::API_VERSION.to_owned(),
                kind: <Service as Resource>::KIND.to_owned(),
                name: t.service,
                container: t.container?,
            })),
            Target::ReplicaSet(t) => Some(Self::KubeResource(KubeResourceTarget {
                api_version: <ReplicaSet as Resource>::API_VERSION.to_owned(),
                kind: <ReplicaSet as Resource>::KIND.to_owned(),
                name: t.replica_set,
                container: t.container?,
            })),
            Target::Label(t) => Some(Self::PodSet(PodSetTarget::new(
                t.labels.into_iter().collect(),
                t.container?,
            ))),
            Target::Targetless => None,
        }
    }

    /// Parse back into a [`Target`] by reconstructing the canonical target path string.
    pub fn into_config(self) -> Option<Target> {
        match self {
            Self::KubeResource(target) => target.into_config(),
            Self::PodSet(target) => {
                let label_selector = LabelSelector::from(target.label_selector);
                if label_selector
                    .match_expressions
                    .is_some_and(|expressions| expressions.is_empty().not())
                {
                    return None;
                }

                Some(Target::Label(LabelTarget {
                    labels: label_selector.match_labels?,
                    container: target
                        .container
                        .is_empty()
                        .not()
                        .then_some(target.container),
                }))
            }
        }
    }
}

mod label_selector_serde {
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
    use kube::core::Selector;
    use schemars::{JsonSchema, Schema, SchemaGenerator};
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};

    use super::PodSetTarget;

    pub fn schema(generator: &mut SchemaGenerator) -> Schema {
        crate::crd::label_selector::LabelSelector::json_schema(generator)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Selector, D::Error>
    where
        D: Deserializer<'de>,
    {
        let selector = LabelSelector::deserialize(deserializer)?;
        Selector::try_from(selector)
            .map(PodSetTarget::normalize)
            .map_err(D::Error::custom)
    }

    pub fn serialize<S>(selector: &Selector, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        LabelSelector::from(selector.clone()).serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use kube::core::Expression;
    use mirrord_config::target::{Target, label::LabelTarget};
    use serde_json::json;

    use super::{KubeResourceTarget, PodSetTarget, SessionTarget};

    #[test]
    fn kube_resource_wire_format_is_unchanged() {
        let value = json!({
            "apiVersion": "apps/v1",
            "kind": "Deployment",
            "name": "otto",
            "container": "otto",
        });

        let target: SessionTarget = serde_json::from_value(value.clone()).unwrap();

        assert_eq!(
            target,
            SessionTarget::KubeResource(KubeResourceTarget {
                api_version: "apps/v1".to_owned(),
                kind: "Deployment".to_owned(),
                name: "otto".to_owned(),
                container: "otto".to_owned(),
            })
        );
        assert_eq!(serde_json::to_value(target).unwrap(), value);
    }

    #[test]
    fn pod_set_uses_kubernetes_label_selector_wire_format() {
        let target = SessionTarget::PodSet(PodSetTarget::new(
            BTreeMap::from([
                ("lestek".to_owned(), "bezprym".to_owned()),
                ("siemowit".to_owned(), "otto".to_owned()),
            ])
            .into_iter()
            .collect(),
            "otto".to_owned(),
        ));

        assert_eq!(
            serde_json::to_value(target).unwrap(),
            json!({
                "labelSelector": {
                    "matchLabels": {
                        "lestek": "bezprym",
                        "siemowit": "otto",
                    },
                },
                "container": "otto",
            })
        );
    }

    #[test]
    fn pod_set_equality_ignores_expression_order() {
        let lhs = PodSetTarget::new(
            [
                Expression::Equal("lestek".to_owned(), "bezprym".to_owned()),
                Expression::Exists("siemowit".to_owned()),
            ]
            .into_iter()
            .collect(),
            "otto".to_owned(),
        );
        let rhs = PodSetTarget::new(
            [
                Expression::Exists("siemowit".to_owned()),
                Expression::Equal("lestek".to_owned(), "bezprym".to_owned()),
            ]
            .into_iter()
            .collect(),
            "otto".to_owned(),
        );

        assert_eq!(lhs, rhs);
    }

    #[test]
    fn label_target_roundtrips_through_pod_set() {
        let config = Target::Label(LabelTarget {
            labels: BTreeMap::from([
                ("lestek".to_owned(), "bezprym".to_owned()),
                ("siemowit".to_owned(), "otto".to_owned()),
            ]),
            container: Some("otto".to_owned()),
        });

        let session_target = SessionTarget::from_config(config.clone()).unwrap();

        assert_eq!(session_target.into_config(), Some(config));
    }

    #[test]
    fn deserialized_pod_set_selector_is_normalized() {
        let target: SessionTarget = serde_json::from_value(json!({
            "labelSelector": {
                "matchExpressions": [
                    { "key": "siemowit", "operator": "Exists" },
                    { "key": "lestek", "operator": "Exists" },
                ],
            },
            "container": "otto",
        }))
        .unwrap();

        let SessionTarget::PodSet(target) = target else {
            panic!("expected pod set target");
        };
        let expressions = target
            .label_selector()
            .clone()
            .into_iter()
            .collect::<Vec<_>>();

        assert_eq!(
            expressions,
            vec![
                Expression::Exists("lestek".to_owned()),
                Expression::Exists("siemowit".to_owned()),
            ]
        );
    }
}

/// Information about the CI session started from `mirrord ci start`.
///
/// We try to get some of these fields automatically, but for some that we cannot, the user may
/// pass them as cli args to `mirrord ci start`, see `cli::ci::StartArgs`.
///
/// These values are passed to the operator, and handled by the `ci_controller`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionCiInfo {
    /// CI provider, e.g. "github", "gitlab", ...
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,

    /// Staging, production, test, nightly, ...
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,

    /// Pipeline/job name, e.g. "e2e-tests".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pipeline: Option<String>,

    /// PR, manual, push, ...
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triggered_by: Option<String>,
}

/// Information about a session started by `mirrord up`.
///
/// These values are passed to the operator through the connect params.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpSessionInfo {
    /// Queue splitting was generated automatically by `mirrord up`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_queue_splitting: Option<bool>,
}
