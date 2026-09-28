//! Step 2 of the `--resource` pipeline: keeping only what the preview pod actually uses.
//!
//! The user points mirrord at the manifests they already maintain, which usually describe much
//! more than one workload. A preview is a copy of one target's pod, so only three things in
//! those files can affect it: the target itself, and the ConfigMaps and Secrets that pod reads.
//! Everything else (an Ingress, another Deployment, a database) is skipped even when it
//! changed, because applying it would change something outside the preview.

use std::collections::BTreeSet;

use serde_json::Value;

use super::{ResourcesError, SuppliedObject};

/// The target a preview copies, as the pipeline matches it against the files.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TargetRef<'a> {
    pub kind: &'a str,
    pub name: &'a str,
    pub container: &'a str,
    pub namespace: &'a str,
}

impl TargetRef<'_> {
    /// `deployment/app`, the form used in every message.
    pub fn display(&self) -> String {
        format!("{}/{}", self.kind.to_lowercase(), self.name)
    }
}

/// Returns the pod template of a workload object, or `None` when `kind` has none.
///
/// A Pod is its own template. Kinds are matched case-insensitively, as `kubectl` does.
pub(crate) fn pod_template(kind: &str, object: &Value) -> Option<Value> {
    let pointer = match kind.to_ascii_lowercase().as_str() {
        "deployment" | "statefulset" | "replicaset" | "daemonset" | "job" | "rollout" => {
            "/spec/template"
        }
        "cronjob" => "/spec/jobTemplate/spec/template",
        "pod" => {
            let mut template = serde_json::Map::new();
            template.insert(
                "metadata".to_owned(),
                object.get("metadata").cloned().unwrap_or(Value::Null),
            );
            template.insert(
                "spec".to_owned(),
                object.get("spec").cloned().unwrap_or(Value::Null),
            );
            return Some(Value::Object(template));
        }
        _ => return None,
    };

    object.pointer(pointer).cloned()
}

/// The workload an Argo Rollout takes its pod template from (`spec.workloadRef`), when it has
/// no template of its own.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct WorkloadRef {
    pub api_version: String,
    pub kind: String,
    pub name: String,
}

pub(crate) fn workload_ref(object: &Value) -> Option<WorkloadRef> {
    if object.pointer("/spec/template").is_some() {
        return None;
    }
    let reference = object.pointer("/spec/workloadRef")?;
    let field = |name: &str| reference.get(name).and_then(Value::as_str);

    Some(WorkloadRef {
        api_version: field("apiVersion").unwrap_or("apps/v1").to_owned(),
        kind: field("kind")?.to_owned(),
        name: field("name")?.to_owned(),
    })
}

/// Whether the pod template runs a container named `container`.
pub(crate) fn has_container(template: &Value, container: &str) -> bool {
    template
        .pointer("/spec/containers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|entry| entry.get("name").and_then(Value::as_str) == Some(container))
}

/// Names of the ConfigMaps and Secrets a pod template reads.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct References {
    pub config_maps: BTreeSet<String>,
    pub secrets: BTreeSet<String>,
}

/// Collects every ConfigMap and Secret the pod uses: through `env` (`configMapKeyRef`,
/// `secretKeyRef`), `envFrom`, and volumes (including projected ones) in regular and init
/// containers, and `imagePullSecrets`, since a changed registry credential decides whether
/// the preview can pull its image at all.
pub(crate) fn references(template: &Value) -> References {
    let mut references = References::default();
    let Some(spec) = template.get("spec") else {
        return references;
    };

    let containers = ["containers", "initContainers"]
        .into_iter()
        .filter_map(|field| spec.get(field).and_then(Value::as_array))
        .flatten();

    for container in containers {
        for env in array(container, "env") {
            let value_from = env.get("valueFrom");
            if let Some(name) = name_at(value_from, "configMapKeyRef", "name") {
                references.config_maps.insert(name);
            }
            if let Some(name) = name_at(value_from, "secretKeyRef", "name") {
                references.secrets.insert(name);
            }
        }

        for env_from in array(container, "envFrom") {
            if let Some(name) = name_at(Some(env_from), "configMapRef", "name") {
                references.config_maps.insert(name);
            }
            if let Some(name) = name_at(Some(env_from), "secretRef", "name") {
                references.secrets.insert(name);
            }
        }
    }

    for pull_secret in array(spec, "imagePullSecrets") {
        if let Some(name) = pull_secret.get("name").and_then(Value::as_str) {
            references.secrets.insert(name.to_owned());
        }
    }

    for volume in array(spec, "volumes") {
        if let Some(name) = name_at(Some(volume), "configMap", "name") {
            references.config_maps.insert(name);
        }
        if let Some(name) = name_at(Some(volume), "secret", "secretName") {
            references.secrets.insert(name);
        }

        let projected = volume.get("projected");
        for source in projected
            .map(|projected| array(projected, "sources"))
            .into_iter()
            .flatten()
        {
            if let Some(name) = name_at(Some(source), "configMap", "name") {
                references.config_maps.insert(name);
            }
            if let Some(name) = name_at(Some(source), "secret", "name") {
                references.secrets.insert(name);
            }
        }
    }

    references
}

fn array<'a>(value: &'a Value, field: &str) -> impl Iterator<Item = &'a Value> {
    value
        .get(field)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn name_at(value: Option<&Value>, object: &str, field: &str) -> Option<String> {
    value?.get(object)?.get(field)?.as_str().map(str::to_owned)
}

/// The objects from the files that can affect the preview, and the ones that cannot.
#[derive(Debug)]
pub(crate) struct Scope<'a> {
    /// The target's own definition, when the files have one.
    pub target: Option<&'a SuppliedObject>,
    pub config_maps: Vec<&'a SuppliedObject>,
    pub secrets: Vec<&'a SuppliedObject>,
    pub out_of_scope: Vec<&'a SuppliedObject>,
}

/// Splits the files' objects into the ones the preview uses and the rest.
///
/// References are read from the target's template in the files when they define it (it is the
/// template the preview will run), and from `live_template` otherwise. An object that names a
/// namespace other than the target's cannot be used by the target's pod and is out of scope.
/// Two definitions of the same in-scope object are an error: which one the user meant is
/// ambiguous.
pub(crate) fn select<'a>(
    objects: &'a [SuppliedObject],
    target: TargetRef<'_>,
    live_template: Option<&Value>,
) -> Result<Scope<'a>, ResourcesError> {
    let in_target_namespace = |object: &SuppliedObject| {
        object
            .namespace
            .as_deref()
            .is_none_or(|namespace| namespace == target.namespace)
    };

    let mut target_definition: Option<&SuppliedObject> = None;
    for object in objects.iter().filter(|object| in_target_namespace(object)) {
        if object.kind.eq_ignore_ascii_case(target.kind) && object.name == target.name {
            reject_duplicate(target_definition, object)?;
            target_definition = Some(object);
        }
    }

    let template = match target_definition {
        Some(definition) => pod_template(&definition.kind, &definition.value),
        None => live_template.cloned(),
    };
    let references = template.as_ref().map(references).unwrap_or_default();

    let mut scope = Scope {
        target: target_definition,
        config_maps: Vec::new(),
        secrets: Vec::new(),
        out_of_scope: Vec::new(),
    };

    for object in objects {
        if target_definition.is_some_and(|definition| std::ptr::eq(definition, object)) {
            continue;
        }

        let in_scope_list = match object.kind.as_str() {
            "ConfigMap" if references.config_maps.contains(&object.name) => {
                Some(&mut scope.config_maps)
            }
            "Secret" if references.secrets.contains(&object.name) => Some(&mut scope.secrets),
            _ => None,
        };

        match in_scope_list {
            Some(list) if in_target_namespace(object) => {
                reject_duplicate(
                    list.iter().copied().find(|seen| seen.name == object.name),
                    object,
                )?;
                list.push(object);
            }
            _ => scope.out_of_scope.push(object),
        }
    }

    Ok(scope)
}

fn reject_duplicate(
    first: Option<&SuppliedObject>,
    second: &SuppliedObject,
) -> Result<(), ResourcesError> {
    match first {
        Some(first) => Err(ResourcesError::Duplicate {
            object: second.display(),
            first: first.source.clone(),
            second: second.source.clone(),
        }),
        None => Ok(()),
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)] // Tests read JSON fixtures with `value[key]`; a panic just fails the test.
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::*;

    fn object(kind: &str, name: &str, value: Value) -> SuppliedObject {
        let mut value = value;
        value["kind"] = json!(kind);
        value["metadata"]["name"] = json!(name);
        SuppliedObject::new(Path::new("./k8s/all.yaml"), 0, value).unwrap()
    }

    fn deployment(name: &str, pod_spec: Value) -> SuppliedObject {
        object(
            "Deployment",
            name,
            json!({"spec": {"template": {"spec": pod_spec}}}),
        )
    }

    const TARGET: TargetRef<'static> = TargetRef {
        kind: "Deployment",
        name: "app",
        container: "app",
        namespace: "default",
    };

    fn app_spec() -> Value {
        json!({
            "containers": [{
                "name": "app",
                "env": [
                    {"name": "A", "valueFrom": {"configMapKeyRef": {"name": "env-cm", "key": "a"}}},
                    {"name": "B", "valueFrom": {"secretKeyRef": {"name": "env-secret", "key": "b"}}},
                ],
                "envFrom": [{"configMapRef": {"name": "from-cm"}}, {"secretRef": {"name": "from-secret"}}],
            }],
            "initContainers": [{"name": "init", "envFrom": [{"configMapRef": {"name": "init-cm"}}]}],
            "volumes": [
                {"name": "v1", "configMap": {"name": "vol-cm"}},
                {"name": "v2", "secret": {"secretName": "vol-secret"}},
                {"name": "v3", "projected": {"sources": [
                    {"configMap": {"name": "proj-cm"}},
                    {"secret": {"name": "proj-secret"}},
                ]}},
            ],
            "imagePullSecrets": [{"name": "registry"}],
        })
    }

    #[test]
    fn references_cover_env_env_from_and_volumes() {
        let references = references(&json!({"spec": app_spec()}));

        assert_eq!(
            references.config_maps,
            ["env-cm", "from-cm", "init-cm", "proj-cm", "vol-cm"]
                .map(str::to_owned)
                .into()
        );
        assert_eq!(
            references.secrets,
            [
                "env-secret",
                "from-secret",
                "proj-secret",
                "registry",
                "vol-secret"
            ]
            .map(str::to_owned)
            .into()
        );
    }

    #[test]
    fn workload_ref_is_read_only_when_the_rollout_has_no_template() {
        let referencing = json!({"spec": {"workloadRef": {"kind": "Deployment", "name": "app"}}});
        assert_eq!(
            workload_ref(&referencing),
            Some(WorkloadRef {
                api_version: "apps/v1".to_owned(),
                kind: "Deployment".to_owned(),
                name: "app".to_owned(),
            })
        );

        let with_template = json!({"spec": {
            "template": {"spec": {}},
            "workloadRef": {"kind": "Deployment", "name": "app"},
        }});
        assert_eq!(workload_ref(&with_template), None);
    }

    #[test]
    fn pod_template_is_found_for_each_workload_kind() {
        let template = json!({"spec": {"containers": [{"name": "app"}]}});
        for kind in ["Deployment", "statefulset", "Rollout", "Job"] {
            let value = json!({"spec": {"template": template}});
            assert_eq!(
                pod_template(kind, &value).as_ref(),
                Some(&template),
                "{kind}"
            );
        }
        let cron = json!({"spec": {"jobTemplate": {"spec": {"template": template}}}});
        assert_eq!(pod_template("CronJob", &cron).as_ref(), Some(&template));

        let pod = json!({"metadata": {"name": "p"}, "spec": {"containers": [{"name": "app"}]}});
        assert!(has_container(&pod_template("Pod", &pod).unwrap(), "app"));

        assert_eq!(pod_template("Ingress", &json!({"spec": {}})), None);
    }

    /// Only the target and what its pod reads are in scope; an unrelated ConfigMap, an Ingress
    /// and another Deployment are skipped even though they sit in the same files.
    #[test]
    fn referenced_objects_are_in_scope_and_unrelated_ones_are_not() {
        let objects = vec![
            deployment("app", app_spec()),
            object("ConfigMap", "vol-cm", json!({"data": {}})),
            object("Secret", "env-secret", json!({"data": {}})),
            object("ConfigMap", "unrelated", json!({"data": {}})),
            object("Ingress", "web", json!({"spec": {}})),
            deployment("other", json!({"containers": [{"name": "x"}]})),
        ];

        let scope = select(&objects, TARGET, None).unwrap();

        assert_eq!(scope.target.map(|target| target.name.as_str()), Some("app"));
        let names = |list: &[&SuppliedObject]| -> Vec<String> {
            list.iter().map(|object| object.display()).collect()
        };
        assert_eq!(names(&scope.config_maps), ["configmap/vol-cm"]);
        assert_eq!(names(&scope.secrets), ["secret/env-secret"]);
        assert_eq!(
            names(&scope.out_of_scope),
            ["configmap/unrelated", "ingress/web", "deployment/other"]
        );
    }

    /// Without the target in the files, the live template decides what is referenced.
    #[test]
    fn live_template_decides_scope_when_the_files_do_not_define_the_target() {
        let objects = vec![
            object("ConfigMap", "vol-cm", json!({"data": {}})),
            object("ConfigMap", "unrelated", json!({"data": {}})),
        ];
        let live = json!({"spec": app_spec()});

        let scope = select(&objects, TARGET, Some(&live)).unwrap();

        assert!(scope.target.is_none());
        assert_eq!(scope.config_maps.len(), 1);
        assert_eq!(scope.out_of_scope.len(), 1);
    }

    #[test]
    fn objects_in_another_namespace_are_out_of_scope() {
        let mut other_namespace = object("ConfigMap", "vol-cm", json!({"data": {}}));
        other_namespace.namespace = Some("prod".to_owned());
        let objects = vec![deployment("app", app_spec()), other_namespace];

        let scope = select(&objects, TARGET, None).unwrap();

        assert!(scope.config_maps.is_empty());
        assert_eq!(scope.out_of_scope.len(), 1);
    }

    #[test]
    fn duplicate_in_scope_definitions_are_rejected() {
        let objects = vec![
            deployment("app", app_spec()),
            object("ConfigMap", "vol-cm", json!({"data": {"a": "1"}})),
            object("ConfigMap", "vol-cm", json!({"data": {"a": "2"}})),
        ];

        let error = select(&objects, TARGET, None).unwrap_err();
        assert!(
            matches!(&error, ResourcesError::Duplicate { object, .. } if object == "configmap/vol-cm"),
            "{error}"
        );
    }
}
