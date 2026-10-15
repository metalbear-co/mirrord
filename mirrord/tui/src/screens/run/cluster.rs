//! Reads what the Run view needs to know about a target from the cluster: its pod spec (for the
//! databases it uses and how it signs in to them), and the operator's split configs and queue
//! registries (for the team's database presets and the queues mirrord can split).
//!
//! Everything here is read-only, and what cannot be read (no permission, a resource type the
//! cluster does not have) is skipped and noted rather than failing the whole inspection.

use std::collections::BTreeMap;

use k8s_openapi::api::{
    apps::v1::{Deployment, ReplicaSet, StatefulSet},
    batch::v1::{CronJob, Job},
    core::v1::{ConfigMap, Container, Pod, PodTemplateSpec, Secret, ServiceAccount},
};
use kube::{
    Api, Client,
    api::{ApiResource, DynamicObject, GroupVersionKind},
};
use mirrord_operator::crd::{MirrordWorkloadQueueRegistry, SplitQueue};
use serde_json::Value;

use super::{
    databases::{self, Database, EnvVar, PodFacts, Source},
    detect::Candidate,
};
use crate::screens::targets::browser::TargetKind;

/// A database the team set up in the target's `MirrordSplitConfig`, ready to branch by id.
#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    pub id: String,
    /// The engine, as the config names it (`pg`, `mysql` ...).
    pub engine: String,
}

/// A queue the operator can split for the target.
#[derive(Clone, Debug, PartialEq)]
pub struct Queue {
    pub id: String,
    /// The broker, such as `sqs` or `kafka`.
    pub kind: String,
}

/// Everything the Run view learned about one target.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TargetResources {
    pub databases: Vec<Database>,
    /// Database-looking URL variables whose engine could not be told.
    pub unidentified: Vec<String>,
    pub presets: Vec<Preset>,
    pub queues: Vec<Queue>,
    /// Things that could not be read, said plainly, so the cards can mention them.
    pub notes: Vec<String>,
}

/// Inspects `candidate` in the cluster.
pub async fn inspect(client: &Client, candidate: &Candidate) -> TargetResources {
    let mut resources = TargetResources::default();
    let namespace = candidate.namespace.as_str();

    let workload = match workload(client, namespace, candidate.kind, &candidate.name).await {
        Ok(workload) => Some(workload),
        Err(error) => {
            resources
                .notes
                .push(format!("could not read the {}: {error}", candidate.kind));
            None
        }
    };

    if let Some((labels, template)) = &workload {
        let facts = pod_facts(
            client,
            namespace,
            template,
            candidate.container.as_deref(),
            &mut resources.notes,
        )
        .await;
        resources.databases = databases::detect(&facts);
        resources.unidentified = databases::unidentified(&facts);

        let (presets, queues) =
            split_configs(client, namespace, candidate, labels, &mut resources.notes).await;
        resources.presets = presets;
        resources.queues = queues;
    }

    for queue in registry_queues(client, namespace, &candidate.name).await {
        if !resources.queues.contains(&queue) {
            resources.queues.push(queue);
        }
    }

    resources
}

/// The workload's own labels and its pod template.
async fn workload(
    client: &Client,
    namespace: &str,
    kind: TargetKind,
    name: &str,
) -> Result<(BTreeMap<String, String>, PodTemplateSpec), String> {
    let error = |error: kube::Error| error.to_string();
    let missing = || "it has no pod template".to_owned();

    let (labels, template) = match kind {
        TargetKind::Deployment => {
            let object = Api::<Deployment>::namespaced(client.clone(), namespace)
                .get(name)
                .await
                .map_err(error)?;
            (
                object.metadata.labels,
                object.spec.map(|spec| spec.template),
            )
        }
        TargetKind::StatefulSet => {
            let object = Api::<StatefulSet>::namespaced(client.clone(), namespace)
                .get(name)
                .await
                .map_err(error)?;
            (
                object.metadata.labels,
                object.spec.map(|spec| spec.template),
            )
        }
        TargetKind::ReplicaSet => {
            let object = Api::<ReplicaSet>::namespaced(client.clone(), namespace)
                .get(name)
                .await
                .map_err(error)?;
            (
                object.metadata.labels,
                object.spec.and_then(|spec| spec.template),
            )
        }
        TargetKind::Job => {
            let object = Api::<Job>::namespaced(client.clone(), namespace)
                .get(name)
                .await
                .map_err(error)?;
            (
                object.metadata.labels,
                object.spec.map(|spec| spec.template),
            )
        }
        TargetKind::CronJob => {
            let object = Api::<CronJob>::namespaced(client.clone(), namespace)
                .get(name)
                .await
                .map_err(error)?;
            (
                object.metadata.labels,
                object
                    .spec
                    .and_then(|spec| spec.job_template.spec)
                    .map(|spec| spec.template),
            )
        }
        TargetKind::Pod => {
            let object = Api::<Pod>::namespaced(client.clone(), namespace)
                .get(name)
                .await
                .map_err(error)?;
            let template = PodTemplateSpec {
                metadata: Some(object.metadata.clone()),
                spec: object.spec,
            };
            (object.metadata.labels, Some(template))
        }
        TargetKind::Rollout => {
            let resource = ApiResource::from_gvk_with_plural(
                &GroupVersionKind::gvk("argoproj.io", "v1alpha1", "Rollout"),
                "rollouts",
            );
            let object =
                Api::<DynamicObject>::namespaced_with(client.clone(), namespace, &resource)
                    .get(name)
                    .await
                    .map_err(error)?;
            let template = object
                .data
                .pointer("/spec/template")
                .cloned()
                .and_then(|template| serde_json::from_value::<PodTemplateSpec>(template).ok());
            (object.metadata.labels, template)
        }
        // A Service has no pod of its own to read.
        TargetKind::Service => return Err("services have no pod spec to read".to_owned()),
    };

    Ok((labels.unwrap_or_default(), template.ok_or_else(missing)?))
}

/// Container names that are never the user's app.
const SIDECARS: &[&str] = &["istio-proxy", "linkerd-proxy", "envoy", "mirrord-agent"];

/// The variables of the targeted container (every app container when none is targeted), with
/// `envFrom` sources expanded into their keys, plus the service account's annotations.
async fn pod_facts(
    client: &Client,
    namespace: &str,
    template: &PodTemplateSpec,
    container: Option<&str>,
    notes: &mut Vec<String>,
) -> PodFacts {
    let Some(spec) = &template.spec else {
        return PodFacts::default();
    };
    let containers: Vec<&Container> = spec
        .containers
        .iter()
        .filter(|candidate| match container {
            Some(name) => candidate.name == name,
            None => !SIDECARS.contains(&candidate.name.as_str()),
        })
        .collect();

    let mut env = Vec::new();
    for container in &containers {
        for var in container.env.iter().flatten() {
            let source = match &var.value_from {
                None => Source::Literal(var.value.clone().unwrap_or_default()),
                Some(from) => match (&from.secret_key_ref, &from.config_map_key_ref) {
                    (Some(secret), _) => Source::Secret {
                        name: secret.name.clone(),
                        key: secret.key.clone(),
                    },
                    (None, Some(config_map)) => Source::ConfigMap {
                        name: config_map.name.clone(),
                        key: config_map.key.clone(),
                    },
                    (None, None) => Source::Other,
                },
            };
            env.push(EnvVar {
                name: var.name.clone(),
                source,
            });
        }

        for from in container.env_from.iter().flatten() {
            let prefix = from.prefix.clone().unwrap_or_default();
            let (kind, name, keys) = if let Some(config_map) = &from.config_map_ref {
                let keys = Api::<ConfigMap>::namespaced(client.clone(), namespace)
                    .get(&config_map.name)
                    .await
                    .map(|object| {
                        object
                            .data
                            .unwrap_or_default()
                            .into_keys()
                            .collect::<Vec<_>>()
                    });
                ("configmap", config_map.name.clone(), keys)
            } else if let Some(secret) = &from.secret_ref {
                // Only the key names are kept: the values the API returns with them are dropped
                // here and never shown or stored.
                let keys = Api::<Secret>::namespaced(client.clone(), namespace)
                    .get(&secret.name)
                    .await
                    .map(|object| {
                        object
                            .data
                            .unwrap_or_default()
                            .into_keys()
                            .collect::<Vec<_>>()
                    });
                ("secret", secret.name.clone(), keys)
            } else {
                continue;
            };

            match keys {
                Ok(keys) => env.extend(keys.into_iter().map(|key| EnvVar {
                    name: format!("{prefix}{key}"),
                    source: Source::EnvFrom {
                        kind,
                        name: name.clone(),
                    },
                })),
                Err(error) => notes.push(format!(
                    "could not read {kind} {name} loaded with envFrom: {error}"
                )),
            }
        }
    }

    let account = spec.service_account_name.as_deref().unwrap_or("default");
    let service_account_annotations = Api::<ServiceAccount>::namespaced(client.clone(), namespace)
        .get(account)
        .await
        .ok()
        .and_then(|account| account.metadata.annotations)
        .unwrap_or_default();

    PodFacts {
        env,
        service_account_annotations,
        containers: spec
            .containers
            .iter()
            .map(|container| container.name.clone())
            .collect(),
    }
}

/// The database presets and queues of every `MirrordSplitConfig` in the namespace that applies
/// to the target, by name or by label selector.
async fn split_configs(
    client: &Client,
    namespace: &str,
    candidate: &Candidate,
    labels: &BTreeMap<String, String>,
    notes: &mut Vec<String>,
) -> (Vec<Preset>, Vec<Queue>) {
    let resource = ApiResource::from_gvk_with_plural(
        &GroupVersionKind::gvk("queues.mirrord.metalbear.co", "v1", "MirrordSplitConfig"),
        "mirrordsplitconfigs",
    );
    let list = match Api::<DynamicObject>::namespaced_with(client.clone(), namespace, &resource)
        .list(&Default::default())
        .await
    {
        Ok(list) => list.items,
        // An operator without split configs, or no permission to read them: nothing to offer.
        Err(kube::Error::Api(status)) if status.code == 404 => return (Vec::new(), Vec::new()),
        Err(error) => {
            notes.push(format!("could not read split configs: {error}"));
            return (Vec::new(), Vec::new());
        }
    };

    let mut presets = Vec::new();
    let mut queues = Vec::new();
    for config in list {
        let spec = config.data.get("spec").cloned().unwrap_or_default();
        if !applies_to(&spec, candidate, labels) {
            continue;
        }
        for branch in array(&spec, "dbBranches") {
            if let Some(id) = branch.get("id").and_then(Value::as_str) {
                presets.push(Preset {
                    id: id.to_owned(),
                    engine: string(branch, "type"),
                });
            }
        }
        for queue in array(&spec, "queues") {
            if let Some(id) = queue.get("id").and_then(Value::as_str) {
                queues.push(Queue {
                    id: id.to_owned(),
                    // `kind` in current split configs; `queueType` in older ones.
                    kind: Some(string(queue, "kind"))
                        .filter(|kind| !kind.is_empty())
                        .unwrap_or_else(|| string(queue, "queueType"))
                        .to_lowercase()
                        .chars()
                        .filter(|char| char.is_alphanumeric())
                        .collect(),
                });
            }
        }
    }
    (presets, queues)
}

/// Whether a split config's spec names the target, by `targetRef` or by a `targetSelector` whose
/// `matchLabels` the workload's labels satisfy.
fn applies_to(spec: &Value, candidate: &Candidate, labels: &BTreeMap<String, String>) -> bool {
    if let Some(target) = spec.get("targetRef") {
        let name_matches =
            target.get("name").and_then(Value::as_str) == Some(candidate.name.as_str());
        let kind_matches = target
            .get("kind")
            .and_then(Value::as_str)
            .is_none_or(|kind| kind.eq_ignore_ascii_case(&candidate.kind.to_string()));
        return name_matches && kind_matches;
    }
    if let Some(selector) = spec.get("targetSelector") {
        let wanted = selector
            .get("matchLabels")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        return !wanted.is_empty()
            && selector.get("matchExpressions").is_none()
            && wanted
                .iter()
                .all(|(key, value)| labels.get(key).map(String::as_str) == value.as_str());
    }
    false
}

/// Queues of the older `MirrordWorkloadQueueRegistry` resources whose consumer is the target.
async fn registry_queues(client: &Client, namespace: &str, target: &str) -> Vec<Queue> {
    let Ok(list) = Api::<MirrordWorkloadQueueRegistry>::namespaced(client.clone(), namespace)
        .list(&Default::default())
        .await
    else {
        return Vec::new();
    };
    list.items
        .into_iter()
        .filter(|registry| registry.spec.consumer.name == target)
        .flat_map(|registry| {
            registry.spec.queues.into_iter().map(|(id, queue)| Queue {
                id: id.to_string(),
                kind: match queue {
                    SplitQueue::Sqs(_) => "sqs",
                    SplitQueue::Rmq(_) => "rabbitmq",
                    SplitQueue::Unknown => "queue",
                }
                .to_owned(),
            })
        })
        .collect()
}

fn array<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    value
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::applies_to;
    use crate::screens::{run::detect::Candidate, targets::browser::TargetKind};

    fn candidate() -> Candidate {
        Candidate {
            namespace: "shop".to_owned(),
            kind: TargetKind::Deployment,
            name: "orders".to_owned(),
            container: None,
            score: 0,
            reason: String::new(),
        }
    }

    #[test]
    fn split_configs_apply_by_name_or_by_labels() {
        let labels = BTreeMap::from([("team".to_owned(), "orders".to_owned())]);

        assert!(applies_to(
            &json!({ "targetRef": { "name": "orders" } }),
            &candidate(),
            &labels
        ));
        assert!(!applies_to(
            &json!({ "targetRef": { "kind": "StatefulSet", "name": "orders" } }),
            &candidate(),
            &labels
        ));
        assert!(applies_to(
            &json!({ "targetSelector": { "matchLabels": { "team": "orders" } } }),
            &candidate(),
            &labels
        ));
        assert!(!applies_to(
            &json!({ "targetSelector": { "matchLabels": { "team": "payments" } } }),
            &candidate(),
            &labels
        ));
    }
}
