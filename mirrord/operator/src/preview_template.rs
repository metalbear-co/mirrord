//! Pod template rules shared by `mirrord preview` and the operator.
//!
//! The CLI decides which manifest objects are in scope for a preview, and it rejects a template
//! that would give the preview more access than the live target, before it replaces a running
//! session. The operator retargets those same references onto the preview's copies and applies
//! the same access limits: a client can create the session directly and skip the CLI.

use std::collections::{BTreeMap, BTreeSet};

use k8s_openapi::api::core::v1::{Container, PodSpec, PodTemplateSpec, Volume};

/// What a pod template reference points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PodReference {
    ConfigMap,
    Secret,
}

/// Calls `visit` with every ConfigMap and Secret name `spec` refers to.
///
/// The sites are `env` key refs, `envFrom`, and volumes (projected ones included) in regular
/// and init containers, plus `imagePullSecrets`. This is the only list: the CLI uses it to
/// decide which manifest objects are in scope, and the operator uses it to retarget those
/// names onto the preview's copies.
pub fn visit_pod_references(spec: &mut PodSpec, mut visit: impl FnMut(PodReference, &mut String)) {
    let containers = spec
        .containers
        .iter_mut()
        .chain(spec.init_containers.iter_mut().flatten());
    for container in containers {
        for env in container.env.iter_mut().flatten() {
            let Some(value_from) = env.value_from.as_mut() else {
                continue;
            };
            if let Some(key_ref) = value_from.config_map_key_ref.as_mut() {
                visit(PodReference::ConfigMap, &mut key_ref.name);
            }
            if let Some(key_ref) = value_from.secret_key_ref.as_mut() {
                visit(PodReference::Secret, &mut key_ref.name);
            }
        }

        for env_from in container.env_from.iter_mut().flatten() {
            if let Some(source) = env_from.config_map_ref.as_mut() {
                visit(PodReference::ConfigMap, &mut source.name);
            }
            if let Some(source) = env_from.secret_ref.as_mut() {
                visit(PodReference::Secret, &mut source.name);
            }
        }
    }

    for pull_secret in spec.image_pull_secrets.iter_mut().flatten() {
        visit(PodReference::Secret, &mut pull_secret.name);
    }

    for volume in spec.volumes.iter_mut().flatten() {
        if let Some(source) = volume.config_map.as_mut() {
            visit(PodReference::ConfigMap, &mut source.name);
        }
        if let Some(name) = volume
            .secret
            .as_mut()
            .and_then(|source| source.secret_name.as_mut())
        {
            visit(PodReference::Secret, name);
        }

        let projected_sources = volume
            .projected
            .as_mut()
            .and_then(|projected| projected.sources.as_mut());
        for source in projected_sources.into_iter().flatten() {
            if let Some(projection) = source.config_map.as_mut() {
                visit(PodReference::ConfigMap, &mut projection.name);
            }
            if let Some(projection) = source.secret.as_mut() {
                visit(PodReference::Secret, &mut projection.name);
            }
        }
    }
}

/// ConfigMap and Secret names [`visit_pod_references`] finds. The walk needs `&mut PodSpec`,
/// so this clones `spec` rather than asking every caller to.
pub fn referenced_names(spec: &PodSpec) -> ReferencedNames {
    let mut names = ReferencedNames::default();
    visit_pod_references(&mut spec.clone(), |kind, name| {
        let set = match kind {
            PodReference::ConfigMap => &mut names.config_maps,
            PodReference::Secret => &mut names.secrets,
        };
        set.insert(name.clone());
    });
    names
}

/// Names collected by [`referenced_names`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReferencedNames {
    pub config_maps: BTreeSet<String>,
    pub secrets: BTreeSet<String>,
}

/// Why a manifest pod template would give the preview more access than the live target.
#[derive(Debug, PartialEq, Eq)]
pub enum TemplateAccessViolation {
    /// `field` decides identity, host access, privileges, or storage, and it differs.
    ChangedField(String),
    /// `secret` is not one the live target uses and not one of `extra_secrets`.
    UnknownSecret(String),
}

/// Rejects a pod template that would give the preview more access than the live target.
///
/// A plain preview already runs the user's image with the target's identity and Secrets, so
/// that is the line: the template may change what runs (images, commands, env, resources,
/// probes, ConfigMaps), not who it runs as, its host access or privileges, the storage it
/// mounts, or the Secrets it reads beyond the target's and `extra_secrets` (the copies of
/// Secrets from the user's own files). A container the target does not have is held to the
/// target container's settings.
pub fn verify_template_access(
    live: &PodTemplateSpec,
    supplied: &PodTemplateSpec,
    extra_secrets: &BTreeSet<String>,
    target_container: Option<&str>,
) -> Result<(), TemplateAccessViolation> {
    let no_spec = PodSpec::default();
    let live = live.spec.as_ref().unwrap_or(&no_spec);
    // Callers that reject a template without a spec do so on their own.
    let Some(supplied) = supplied.spec.as_ref() else {
        return Ok(());
    };

    if let Some(field) = changed_pod_field(live, supplied) {
        return Err(TemplateAccessViolation::ChangedField(field.to_owned()));
    }

    let live_containers: BTreeMap<&str, &Container> = live
        .containers
        .iter()
        .chain(live.init_containers.iter().flatten())
        .map(|container| (container.name.as_str(), container))
        .collect();
    let no_container = Container::default();
    let baseline = target_container
        .and_then(|name| live_containers.get(name).copied())
        .unwrap_or(&no_container);
    let supplied_containers = supplied
        .containers
        .iter()
        .chain(supplied.init_containers.iter().flatten());
    for container in supplied_containers {
        let live_container = live_containers
            .get(container.name.as_str())
            .copied()
            .unwrap_or(baseline);
        if let Some(field) = changed_container_field(live_container, container) {
            return Err(TemplateAccessViolation::ChangedField(format!(
                "containers[{}].{field}",
                container.name
            )));
        }
    }

    let live_storage: Vec<Volume> = live
        .volumes
        .iter()
        .flatten()
        .filter(|volume| !is_pod_local(volume))
        .map(storage_source)
        .collect();
    let new_storage =
        supplied.volumes.iter().flatten().find(|volume| {
            !is_pod_local(volume) && !live_storage.contains(&storage_source(volume))
        });
    if let Some(volume) = new_storage {
        return Err(TemplateAccessViolation::ChangedField(format!(
            "volumes[{}]",
            volume.name
        )));
    }

    let allowed: BTreeSet<_> = referenced_names(live)
        .secrets
        .into_iter()
        .chain(extra_secrets.iter().cloned())
        .collect();
    match referenced_names(supplied)
        .secrets
        .into_iter()
        .find(|name| !allowed.contains(name))
    {
        Some(secret) => Err(TemplateAccessViolation::UnknownSecret(secret)),
        None => Ok(()),
    }
}

/// `None` and the default are the same setting: the API server writes some defaults into the
/// live template (`securityContext: {}`) that files leave out.
fn configured<T: Default + PartialEq>(value: &Option<T>) -> Option<&T> {
    value.as_ref().filter(|value| **value != T::default())
}

/// The first pod-level field deciding identity, host access or privileges that differs.
fn changed_pod_field(live: &PodSpec, supplied: &PodSpec) -> Option<&'static str> {
    // `serviceAccount` is the deprecated alias the API server falls back to.
    let service_account = |spec: &PodSpec| {
        spec.service_account_name
            .as_deref()
            .or(spec.service_account.as_deref())
            .unwrap_or("default")
            .to_owned()
    };
    let host = |flag: Option<bool>| flag.unwrap_or(false);
    [
        (
            "serviceAccountName",
            service_account(live) == service_account(supplied),
        ),
        (
            "automountServiceAccountToken",
            live.automount_service_account_token == supplied.automount_service_account_token,
        ),
        (
            "hostNetwork",
            host(live.host_network) == host(supplied.host_network),
        ),
        ("hostPID", host(live.host_pid) == host(supplied.host_pid)),
        ("hostIPC", host(live.host_ipc) == host(supplied.host_ipc)),
        (
            "hostUsers",
            live.host_users.unwrap_or(true) == supplied.host_users.unwrap_or(true),
        ),
        (
            "securityContext",
            configured(&live.security_context) == configured(&supplied.security_context),
        ),
        (
            "runtimeClassName",
            live.runtime_class_name == supplied.runtime_class_name,
        ),
    ]
    .into_iter()
    .find_map(|(field, same)| (!same).then_some(field))
}

/// The first container field deciding privileges or host access that differs.
fn changed_container_field(live: &Container, supplied: &Container) -> Option<&'static str> {
    let host_ports = |container: &Container| {
        container
            .ports
            .iter()
            .flatten()
            .filter_map(|port| port.host_port)
            .collect::<BTreeSet<_>>()
    };
    [
        (
            "securityContext",
            configured(&live.security_context) == configured(&supplied.security_context),
        ),
        ("ports[].hostPort", host_ports(live) == host_ports(supplied)),
        (
            "volumeDevices",
            live.volume_devices == supplied.volume_devices,
        ),
    ]
    .into_iter()
    .find_map(|(field, same)| (!same).then_some(field))
}

/// Whether `volume` holds only the pod's own files, ConfigMaps or Secrets (Secret references
/// are checked on their own), so the user may add or change it. Anything else (a node path, a
/// claim, network storage, a service account token) must match storage the target mounts.
fn is_pod_local(volume: &Volume) -> bool {
    let projects_files_only = volume.projected.as_ref().is_none_or(|projected| {
        projected.sources.iter().flatten().all(|source| {
            source.service_account_token.is_none() && source.cluster_trust_bundle.is_none()
        })
    });
    let other_sources = Volume {
        config_map: None,
        secret: None,
        empty_dir: None,
        downward_api: None,
        projected: None,
        ..volume.clone()
    };
    projects_files_only
        && other_sources
            == Volume {
                name: volume.name.clone(),
                ..Default::default()
            }
}

/// `volume` without its name, which only ties it to the pod's mounts, and with the `hostPath`
/// type the API server fills in on the live object (`""`) read as unset.
fn storage_source(volume: &Volume) -> Volume {
    let mut source = Volume {
        name: String::new(),
        ..volume.clone()
    };
    if let Some(host_path) = source.host_path.as_mut() {
        host_path.type_ = host_path.type_.take().filter(|type_| !type_.is_empty());
    }
    source
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(spec: serde_json::Value) -> PodTemplateSpec {
        serde_json::from_value(serde_json::json!({ "spec": spec })).unwrap()
    }

    /// The live target: its own ServiceAccount, one Secret, one ConfigMap and one claim.
    fn live_template() -> PodTemplateSpec {
        template(serde_json::json!({
            "serviceAccountName": "app",
            "securityContext": {},
            "containers": [{
                "name": "app",
                "image": "app:1",
                "envFrom": [{"secretRef": {"name": "app-secret"}}, {"configMapRef": {"name": "app-config"}}],
            }],
            "volumes": [{"name": "data", "persistentVolumeClaim": {"claimName": "app-data"}}],
        }))
    }

    fn verify(
        supplied: serde_json::Value,
        extra_secrets: &[&str],
    ) -> Result<(), TemplateAccessViolation> {
        let extra_secrets = extra_secrets
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        verify_template_access(
            &live_template(),
            &template(supplied),
            &extra_secrets,
            Some("app"),
        )
    }

    fn changed_field(result: Result<(), TemplateAccessViolation>) -> String {
        match result {
            Err(TemplateAccessViolation::ChangedField(field)) => field,
            other => panic!("expected a changed field, got {other:?}"),
        }
    }

    /// What runs may change freely: image, env, ConfigMaps, the claim under another volume name.
    /// A file that leaves out the `securityContext: {}` the API server wrote on the live object
    /// is the same setting.
    #[test]
    fn template_changing_what_runs_is_accepted() {
        let result = verify(
            serde_json::json!({
                "serviceAccountName": "app",
                "containers": [{
                    "name": "app",
                    "image": "app:2",
                    "env": [{"name": "LEVEL", "value": "debug"}],
                    "envFrom": [{"secretRef": {"name": "app-secret"}}, {"configMapRef": {"name": "other-config"}}],
                }],
                "volumes": [
                    {"name": "renamed", "persistentVolumeClaim": {"claimName": "app-data"}},
                    {"name": "scratch", "emptyDir": {}},
                ],
            }),
            &[],
        );

        assert!(result.is_ok(), "{result:?}");
    }

    /// A preview must not run as another ServiceAccount than the target, however the template
    /// names it.
    #[test]
    fn other_service_account_is_rejected() {
        let field = changed_field(verify(
            serde_json::json!({
                "serviceAccount": "admin",
                "containers": [{"name": "app"}],
            }),
            &[],
        ));

        assert_eq!(field, "serviceAccountName");
    }

    /// A sidecar the target does not have gets the target container's privileges, nothing more.
    #[test]
    fn privileged_sidecar_is_rejected() {
        let field = changed_field(verify(
            serde_json::json!({
                "serviceAccountName": "app",
                "containers": [
                    {"name": "app"},
                    {"name": "debug", "securityContext": {"privileged": true}},
                ],
            }),
            &[],
        ));

        assert_eq!(field, "containers[debug].securityContext");
    }

    /// Storage the target does not mount, like a node path, is out of reach.
    #[test]
    fn host_path_volume_is_rejected() {
        let field = changed_field(verify(
            serde_json::json!({
                "serviceAccountName": "app",
                "containers": [{"name": "app"}],
                "volumes": [{"name": "root", "hostPath": {"path": "/"}}],
            }),
            &[],
        ));

        assert_eq!(field, "volumes[root]");
    }

    /// The API server fills in `type: ""` on a live `hostPath` volume that files leave out, so
    /// the target's own node path still matches.
    #[test]
    fn live_host_path_with_filled_in_type_matches() {
        let live = template(serde_json::json!({
            "containers": [{"name": "app"}],
            "volumes": [{"name": "logs", "hostPath": {"path": "/var/log", "type": ""}}],
        }));
        let supplied = template(serde_json::json!({
            "containers": [{"name": "app", "image": "app:2"}],
            "volumes": [{"name": "logs", "hostPath": {"path": "/var/log"}}],
        }));

        let result = verify_template_access(&live, &supplied, &BTreeSet::new(), Some("app"));

        assert!(result.is_ok(), "{result:?}");
    }

    /// A Secret the target does not use is readable only as a copy from the user's own files.
    #[test]
    fn unrelated_secret_is_rejected_unless_the_files_carry_it() {
        let supplied = serde_json::json!({
            "serviceAccountName": "app",
            "containers": [{
                "name": "app",
                "env": [{
                    "name": "TOKEN",
                    "valueFrom": {"secretKeyRef": {"name": "billing-token", "key": "token"}},
                }],
            }],
        });

        let result = verify(supplied.clone(), &[]);
        assert!(
            matches!(&result, Err(TemplateAccessViolation::UnknownSecret(secret)) if secret == "billing-token"),
            "{result:?}"
        );

        let result = verify(supplied, &["billing-token"]);
        assert!(result.is_ok(), "{result:?}");
    }
}
