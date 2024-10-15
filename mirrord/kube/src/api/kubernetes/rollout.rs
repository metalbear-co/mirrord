use std::borrow::Cow;

use k8s_openapi::{
    api::{
        apps::v1::{Deployment, ReplicaSet, StatefulSet},
        core::v1::{PodTemplate, PodTemplateSpec},
    },
    apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta},
    ListableResource, Metadata, NamespaceResourceScope, Resource,
};
use kube::Client;
use serde::{de, Deserialize, Serialize};

use super::get_k8s_resource_api;
use crate::error::KubeApiError;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rollout {
    pub metadata: ObjectMeta,
    pub spec: Option<RolloutSpec>,
    pub status: Option<RolloutStatus>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RolloutStatus {
    pub available_replicas: Option<i32>,
    /// Looks like this is a string for some reason:
    /// [rollouts/v1alpha1/types.go](https://github.com/argoproj/argo-rollouts/blob/4f1edbe9332b93d8aaf1d8f34239da6f952b8a93/pkg/apis/rollouts/v1alpha1/types.go#L922)
    pub observed_generation: Option<String>,
}

/// Argo [`Rollout`]s provide `Pod` template in one of two ways:
/// 1. Inline (`template` field).
/// 2. Via a reference to some Kubernetes workload (`workloadRef` field).
///
/// See [Rollout spec](https://argoproj.github.io/argo-rollouts/features/specification/) for reference.
#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RolloutSpec {
    pub selector: LabelSelector,
    #[serde(deserialize_with = "rollout_pod_spec")]
    pub template: Option<PodTemplateSpec>,
    pub workload_ref: Option<WorkloadRef>,
}

/// A reference to some Kubernetes [workload](https://kubernetes.io/docs/concepts/workloads/) managed by an Argo [`Rollout`].
///
/// The documentation of Argo do not mention any restrictions no the referenced resource type -
/// "WorkloadRef holds a references to a workload that provides Pod template".
///
/// # Note
///
/// Information contained in this struct is not enough to fetch a dynamic resource (see
/// [`ApiResource`](kube::discovery::ApiResource)). What's more, there would be no way of knowing
/// how to extract the required [`PodTemplateSpec`] from the fetched resource.
///
/// Luckily, [source code](https://github.com/argoproj/argo-rollouts/blob/master/rollout/templateref.go#L41) provides constraints on the resource type.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadRef {
    pub api_version: String,
    pub kind: String,
    pub name: String,
}

/// Custom deserializer for a rollout template field due to
/// [#548](https://github.com/metalbear-co/operator/issues/548)
/// First deserializes it as value, fixes possible issues and then deserializes it as
/// PodTemplateSpec.
fn rollout_pod_spec<'de, D>(deserializer: D) -> Result<Option<PodTemplateSpec>, D::Error>
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

    let pod_template: PodTemplateSpec = serde_json::from_value(value).map_err(de::Error::custom)?;
    Ok(Some(pod_template))
}

impl Rollout {
    /// Get the pod template spec out of a rollout spec.
    /// Make requests to k8s if necessary when the template is not directly included in the
    /// rollout spec ([`RolloutSpec::template`]), but only referenced via a workload_ref
    /// ([`RolloutSpec::workload_ref`]).
    pub async fn get_pod_template<'a>(
        &'a self,
        client: &Client,
    ) -> Result<Cow<'a, PodTemplateSpec>, KubeApiError> {
        let spec = self
            .spec
            .as_ref()
            .ok_or_else(|| KubeApiError::missing_field(self, ".spec"))?;

        match spec {
            RolloutSpec {
                template: Some(..),
                workload_ref: Some(..),
                ..
            } => Err(KubeApiError::invalid_state(
                self,
                "both `.spec.template` and `.spec.workladRef` fields are filled",
            )),

            RolloutSpec {
                template: None,
                workload_ref: None,
                ..
            } => Err(KubeApiError::invalid_state(
                self,
                "both `.spec.template` and `.spec.workloadRef` fields are empty",
            )),

            RolloutSpec {
                template: Some(template),
                ..
            } => Ok(Cow::Borrowed(template)),

            RolloutSpec {
                workload_ref: Some(workload_ref),
                ..
            } => workload_ref
                .get_pod_template(client, self.metadata.namespace.as_deref())
                .await?
                .ok_or_else(|| {
                    KubeApiError::invalid_state(
                        self,
                        format_args!(
                            "field `.spec.workloadRef` refers to an unknown resource `{}/{}`",
                            workload_ref.api_version, workload_ref.kind
                        ),
                    )
                })
                .map(Cow::Owned),
        }
    }
}

impl WorkloadRef {
    /// Fetched the referenced resource and extracts [`PodTemplateSpec`].
    /// Supports references to:
    /// 1. [`Deployment`]s
    /// 2. [`ReplicaSet`]s
    /// 3. [`PodTemplate`]s
    /// 4. [`StatefulSet`]s
    pub async fn get_pod_template(
        &self,
        client: &Client,
        namespace: Option<&str>,
    ) -> Result<Option<PodTemplateSpec>, KubeApiError> {
        match (self.api_version.as_str(), self.kind.as_str()) {
            (Deployment::API_VERSION, Deployment::KIND) => {
                let mut deployment = get_k8s_resource_api::<Deployment>(client, namespace)
                    .get(&self.name)
                    .await?;

                deployment
                    .spec
                    .take()
                    .ok_or_else(|| KubeApiError::missing_field(&deployment, ".spec"))
                    .map(|spec| Some(spec.template))
            }
            (ReplicaSet::API_VERSION, ReplicaSet::KIND) => {
                let mut replica_set = get_k8s_resource_api::<ReplicaSet>(client, namespace)
                    .get(&self.name)
                    .await?;

                replica_set
                    .spec
                    .take()
                    .ok_or_else(|| KubeApiError::missing_field(&replica_set, ".spec"))?
                    .template
                    .ok_or_else(|| KubeApiError::missing_field(&replica_set, ".spec.template"))
                    .map(Some)
            }
            (PodTemplate::API_VERSION, PodTemplate::KIND) => {
                let mut pod_template = get_k8s_resource_api::<PodTemplate>(client, namespace)
                    .get(&self.name)
                    .await?;

                pod_template
                    .template
                    .take()
                    .ok_or_else(|| KubeApiError::missing_field(&pod_template, ".template"))
                    .map(Some)
            }
            (StatefulSet::API_VERSION, StatefulSet::KIND) => {
                let mut stateful_set = get_k8s_resource_api::<StatefulSet>(client, namespace)
                    .get(&self.name)
                    .await?;

                stateful_set
                    .spec
                    .take()
                    .ok_or_else(|| KubeApiError::missing_field(&stateful_set, ".spec"))
                    .map(|spec| Some(spec.template))
            }
            _ => Ok(None),
        }
    }
}

impl Resource for Rollout {
    const API_VERSION: &'static str = "argoproj.io/v1alpha1";
    const GROUP: &'static str = "argoproj.io";
    const KIND: &'static str = "Rollout";
    const VERSION: &'static str = "v1alpha1";
    const URL_PATH_SEGMENT: &'static str = "rollouts";
    type Scope = NamespaceResourceScope;
}

impl ListableResource for Rollout {
    const LIST_KIND: &'static str = "RolloutList";
}

impl Metadata for Rollout {
    type Ty = ObjectMeta;

    fn metadata(&self) -> &Self::Ty {
        &self.metadata
    }

    fn metadata_mut(&mut self) -> &mut Self::Ty {
        &mut self.metadata
    }
}

#[cfg(test)]
pub mod test {
    #[test]
    fn test_rollout() {
        let raw_json = r#"{
  "apiVersion": "argoproj.io/v1alpha1",
  "kind": "Rollout",
  "metadata": {
    "annotations": {
      "kubectl.kubernetes.io/last-applied-configuration": "{}\n",
      "rollout.argoproj.io/revision": "19"
    },
    "creationTimestamp": "2024-04-17T19:35:25Z",
    "generation": 41,
    "labels": {
      "app.kubernetes.io/instance": "test-web-stage",
      "app.kubernetes.io/managed-by": "Helm",
      "app.kubernetes.io/name": "test-web",
      "app.kubernetes.io/version": "1.929.0",
      "argocd.argoproj.io/instance": "gl-test-web-stage-e2",
      "helm.sh/chart": "test-web-1.929.0",
      "tags.datadoghq.com/env": "staging",
      "tags.datadoghq.com/test-web.env": "staging",
      "tags.datadoghq.com/test-web.service": "test-web",
      "tags.datadoghq.com/test-web.version": "1.929.0",
      "tags.datadoghq.com/service": "test-web",
      "tags.datadoghq.com/version": "1.929.0"
    },
    "managedFields": [
      {
        "apiVersion": "argoproj.io/v1alpha1",
        "fieldsType": "FieldsV1",
        "fieldsV1": {
          "f:spec": {
            "f:replicas": {}
          }
        },
        "manager": "kube-controller-manager",
        "operation": "Update",
        "subresource": "scale"
      },
      {
        "apiVersion": "argoproj.io/v1alpha1",
        "fieldsType": "FieldsV1",
        "fieldsV1": {
          "f:metadata": {
            "f:annotations": {
              "f:rollout.argoproj.io/revision": {}
            }
          }
        },
        "manager": "rollouts-controller",
        "operation": "Update",
        "time": "2024-06-10T16:08:41Z"
      },
      {
        "apiVersion": "argoproj.io/v1alpha1",
        "fieldsType": "FieldsV1",
        "fieldsV1": {
          "f:metadata": {
            "f:annotations": {
              ".": {},
              "f:kubectl.kubernetes.io/last-applied-configuration": {}
            },
            "f:labels": {
              ".": {},
              "f:app.kubernetes.io/instance": {},
              "f:app.kubernetes.io/managed-by": {},
              "f:app.kubernetes.io/name": {},
              "f:app.kubernetes.io/version": {},
              "f:argocd.argoproj.io/instance": {},
              "f:helm.sh/chart": {},
              "f:tags.datadoghq.com/env": {},
              "f:tags.datadoghq.com/test-web.env": {},
              "f:tags.datadoghq.com/test-web.service": {},
              "f:tags.datadoghq.com/test-web.version": {},
              "f:tags.datadoghq.com/service": {},
              "f:tags.datadoghq.com/version": {}
            }
          },
          "f:spec": {
            ".": {},
            "f:revisionHistoryLimit": {},
            "f:selector": {
              ".": {},
              "f:matchLabels": {
                ".": {},
                "f:app.kubernetes.io/instance": {},
                "f:app.kubernetes.io/name": {}
              }
            },
            "f:strategy": {
              ".": {},
              "f:canary": {
                ".": {},
                "f:analysis": {
                  ".": {},
                  "f:templates": {}
                },
                "f:maxSurge": {},
                "f:maxUnavailable": {},
                "f:steps": {}
              }
            },
            "f:template": {
              ".": {},
              "f:metadata": {
                ".": {},
                "f:annotations": {
                  ".": {},
                  "f:ad.datadoghq.com/test-web.check_names": {},
                  "f:ad.datadoghq.com/test-web.init_configs": {},
                  "f:ad.datadoghq.com/test-web.instances": {},
                  "f:ad.datadoghq.com/test-web.tags": {}
                },
                "f:labels": {
                  ".": {},
                  "f:app.kubernetes.io/instance": {},
                  "f:app.kubernetes.io/name": {},
                  "f:tags.datadoghq.com/env": {},
                  "f:tags.datadoghq.com/test-web.env": {},
                  "f:tags.datadoghq.com/test-web.service": {},
                  "f:tags.datadoghq.com/test-web.version": {},
                  "f:tags.datadoghq.com/service": {},
                  "f:tags.datadoghq.com/version": {}
                }
              },
              "f:spec": {
                ".": {},
                "f:affinity": {
                  ".": {},
                  "f:podAntiAffinity": {
                    ".": {},
                    "f:preferredDuringSchedulingIgnoredDuringExecution": {}
                  }
                },
                "f:containers": {},
                "f:imagePullSecrets": {},
                "f:serviceAccountName": {}
              }
            }
          }
        },
        "manager": "argocd-controller",
        "operation": "Update",
        "time": "2024-06-10T16:09:32Z"
      },
      {
        "apiVersion": "argoproj.io/v1alpha1",
        "fieldsType": "FieldsV1",
        "fieldsV1": {
          "f:status": {
            ".": {},
            "f:HPAReplicas": {},
            "f:availableReplicas": {},
            "f:blueGreen": {},
            "f:canary": {},
            "f:conditions": {},
            "f:currentPodHash": {},
            "f:currentStepHash": {},
            "f:currentStepIndex": {},
            "f:observedGeneration": {},
            "f:phase": {},
            "f:readyReplicas": {},
            "f:replicas": {},
            "f:selector": {},
            "f:stableRS": {},
            "f:updatedReplicas": {}
          }
        },
        "manager": "rollouts-controller",
        "operation": "Update",
        "subresource": "status",
        "time": "2024-06-11T11:30:40Z"
      }
    ],
    "name": "test-web",
    "namespace": "stage",
    "resourceVersion": "1861976373",
    "uid": "8eba1f7b-9e3b-4cb7-93fe-ce6a26c27686",
    "selfLink": "/apis/argoproj.io/v1alpha1/namespaces/stage/rollouts/test-web"
  },
  "status": {
    "HPAReplicas": 2,
    "availableReplicas": 2,
    "blueGreen": {},
    "canary": {},
    "conditions": [
      {
        "lastTransitionTime": "2024-06-10T16:16:21Z",
        "lastUpdateTime": "2024-06-10T16:16:21Z",
        "message": "RolloutCompleted",
        "reason": "RolloutCompleted",
        "status": "True",
        "type": "Completed"
      },
      {
        "lastTransitionTime": "2024-06-10T16:16:21Z",
        "lastUpdateTime": "2024-06-10T16:16:21Z",
        "message": "Rollout is paused",
        "reason": "RolloutPaused",
        "status": "False",
        "type": "Paused"
      },
      {
        "lastTransitionTime": "2024-06-11T11:30:40Z",
        "lastUpdateTime": "2024-06-11T11:30:40Z",
        "message": "Rollout is healthy",
        "reason": "RolloutHealthy",
        "status": "True",
        "type": "Healthy"
      },
      {
        "lastTransitionTime": "2024-06-10T16:16:21Z",
        "lastUpdateTime": "2024-06-11T11:30:40Z",
        "message": "ReplicaSet \"test-web-7ff8567587\" has successfully progressed.",
        "reason": "NewReplicaSetAvailable",
        "status": "True",
        "type": "Progressing"
      },
      {
        "lastTransitionTime": "2024-06-11T11:30:40Z",
        "lastUpdateTime": "2024-06-11T11:30:40Z",
        "message": "Rollout has minimum availability",
        "reason": "AvailableReason",
        "status": "True",
        "type": "Available"
      }
    ],
    "currentPodHash": "7ff8567587",
    "currentStepHash": "f847f885c",
    "currentStepIndex": 6,
    "observedGeneration": "41",
    "phase": "Healthy",
    "readyReplicas": 2,
    "replicas": 2,
    "selector": "app.kubernetes.io/instance=test-web-stage,app.kubernetes.io/name=test-web",
    "stableRS": "7ff8567587",
    "updatedReplicas": 2
  },
  "spec": {
    "replicas": 2,
    "revisionHistoryLimit": 1,
    "selector": {
      "matchLabels": {
        "app.kubernetes.io/instance": "test-web-stage",
        "app.kubernetes.io/name": "test-web"
      }
    },
    "strategy": {
      "canary": {
        "analysis": {
          "templates": [
            {
              "templateName": "test-web-express-http-success-rate"
            }
          ]
        },
        "maxSurge": "5%",
        "maxUnavailable": "5%",
        "steps": [
          {
            "setWeight": 25
          },
          {
            "pause": {
              "duration": "2m"
            }
          },
          {
            "setWeight": 50
          },
          {
            "pause": {
              "duration": "2m"
            }
          },
          {
            "setWeight": 100
          },
          {
            "pause": {
              "duration": "2m"
            }
          }
        ]
      }
    },
    "template": {
      "metadata": {
        "labels": {
          "app.kubernetes.io/instance": "test-web-stage",
          "app.kubernetes.io/name": "test-web",
          "tags.datadoghq.com/env": "staging",
          "tags.datadoghq.com/test-web.env": "staging",
          "tags.datadoghq.com/test-web.service": "test-web",
          "tags.datadoghq.com/test-web.version": "1.929.0",
          "tags.datadoghq.com/service": "test-web",
          "tags.datadoghq.com/version": "1.929.0"
        }
      },
      "spec": {
        "affinity": {
          "podAntiAffinity": {
            "preferredDuringSchedulingIgnoredDuringExecution": [
              {
                "podAffinityTerm": {
                  "labelSelector": {
                    "matchExpressions": [
                      {
                        "key": "app.kubernetes.io/name",
                        "operator": "In",
                        "values": [
                          "test-web"
                        ]
                      }
                    ]
                  },
                  "topologyKey": "failure-domain.beta.kubernetes.io/zone"
                },
                "weight": 100
              }
            ]
          }
        },
        "containers": [
          {
            "image": "shbd-docker.jfrog.io/test-web:1.929.0",
            "imagePullPolicy": "IfNotPresent",
            "livenessProbe": {
              "httpGet": {
                "path": "/healthz",
                "port": "http"
              },
              "initialDelaySeconds": 30,
              "periodSeconds": 10
            },
            "name": "test-web",
            "ports": [
              {
                "containerPort": 3000,
                "name": "http",
                "protocol": "TCP"
              }
            ],
            "readinessProbe": {
              "httpGet": {
                "path": "/healthz",
                "port": "http"
              },
              "initialDelaySeconds": 30,
              "periodSeconds": 5
            },
            "resources": {
              "limits": {
                "cpu": 1,
                "memory": "1024Mi"
              },
              "requests": {
                "cpu": "500m",
                "memory": "512Mi"
              }
            },
            "startupProbe": {
              "httpGet": {
                "path": "/healthz",
                "port": "http"
              },
              "initialDelaySeconds": 10,
              "periodSeconds": 10
            }
          }
        ],
        "imagePullSecrets": [
          {
            "name": "pp-docker"
          }
        ],
        "serviceAccountName": "test-web"
      }
    }
  }
}"#;

        let _rollout: super::Rollout = serde_json::from_str(raw_json).unwrap();
    }
}
