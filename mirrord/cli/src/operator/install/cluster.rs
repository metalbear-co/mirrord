//! Cluster side of the installation: checking for an existing operator, creating the manifest's
//! objects, and waiting for the operator to come up.

use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    ops::Not,
    time::Duration,
};

use http::StatusCode;
use k8s_openapi::{
    api::core::v1::Namespace, kube_aggregator::pkg::apis::apiregistration::v1::APIService,
};
use kube::{
    Api, Client, ResourceExt,
    api::{DynamicObject, Patch, PatchParams, PostParams},
    core::GroupVersionKind,
    discovery::{self, Scope},
};
use mirrord_operator::crd::{MirrordOperatorCrd, OPERATOR_STATUS_NAME};
use tokio::time::Instant;

use super::{
    error::OperatorInstallError,
    manifest::{self, Manifest},
};

/// Registers the operator API with the Kubernetes API server, present as long as an operator is
/// installed.
const OPERATOR_API_SERVICE: &str = "v1.operator.metalbear.co";

/// The field manager helm 4 applies releases with.
///
/// Applying under the same manager lets the printed `helm install` take over the objects. Under a
/// different one, fields the API server defaults inside atomic values (e.g. RBAC subjects) would
/// conflict with helm's apply, even though the rendered manifest is identical.
const FIELD_MANAGER: &str = "helm";

const READY_TIMEOUT: Duration = Duration::from_secs(5 * 60);

const READY_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Identifies the cluster by the UID of its `default` namespace, which is stable for the lifetime
/// of the cluster and is the same ID the operator reports for it.
///
/// Used as the trial's cluster hint in place of the kubecontext name, which can carry the user's
/// account identity (e.g. EKS context names are ARNs with the AWS account ID, GKE ones name the
/// project) and must not be sent without consent. `None` if the namespace can't be read.
pub(super) async fn cluster_id(client: &Client) -> Option<String> {
    Api::<Namespace>::all(client.clone())
        .get("default")
        .await
        .inspect_err(|error| tracing::debug!(?error, "failed to fetch the cluster ID"))
        .ok()
        .and_then(|namespace| namespace.metadata.uid)
}

/// Resolves the API of every manifest object, in manifest order.
///
/// Namespaced objects without a namespace go to `release_namespace`, as with `helm install`.
pub(super) async fn resolve_apis(
    client: &Client,
    manifest: &Manifest,
    release_namespace: &str,
) -> Result<Vec<Api<DynamicObject>>, OperatorInstallError> {
    let mut resources = HashMap::new();
    let mut apis = Vec::with_capacity(manifest.objects().len());

    for object in manifest.objects() {
        let types = object
            .types
            .as_ref()
            .ok_or_else(|| OperatorInstallError::UntypedObject {
                name: object.name_any(),
            })?;
        let gvk =
            GroupVersionKind::try_from(types).map_err(OperatorInstallError::InvalidObjectType)?;

        let (resource, capabilities) = match resources.entry(gvk) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let resolved =
                    discovery::pinned_kind(client, entry.key())
                        .await
                        .map_err(|source| OperatorInstallError::Discovery {
                            object: describe(object),
                            source: Box::new(source),
                        })?;
                entry.insert(resolved)
            }
        };

        let api = match capabilities.scope {
            Scope::Cluster => Api::all_with(client.clone(), resource),
            Scope::Namespaced => Api::namespaced_with(
                client.clone(),
                object.namespace().as_deref().unwrap_or(release_namespace),
                resource,
            ),
        };
        apis.push(api);
    }

    Ok(apis)
}

/// Stops the installation if an operator is already registered in the cluster, whether it's
/// healthy or not. `context_arg` gives the commands in the error the kubecontext of the run.
pub(super) async fn ensure_no_operator(
    client: &Client,
    context_arg: &str,
) -> Result<(), OperatorInstallError> {
    let Some(api_service) = Api::<APIService>::all(client.clone())
        .get_opt(OPERATOR_API_SERVICE)
        .await
        .map_err(|error| OperatorInstallError::Preflight(Box::new(error)))?
    else {
        return Ok(());
    };

    let namespace = api_service
        .spec
        .and_then(|spec| spec.service)
        .and_then(|service| service.namespace)
        .unwrap_or_else(|| "<unknown>".to_owned());

    match Api::<MirrordOperatorCrd>::all(client.clone())
        .get(OPERATOR_STATUS_NAME)
        .await
    {
        Ok(operator) => Err(OperatorInstallError::AlreadyInstalled {
            namespace,
            version: operator.spec.operator_version,
        }),
        Err(error) => Err(OperatorInstallError::Unhealthy {
            namespace,
            context_arg: context_arg.to_owned(),
            source: Box::new(error),
        }),
    }
}

/// Dry-runs creating every object, so missing permissions, leftovers of an earlier installation and
/// objects of other tools surface before anything is created or a trial is started.
///
/// A plain create finds the leftovers, which a server-side apply would silently merge into. Since
/// [`create`] applies server-side, which also needs the `patch` permission, each object is then
/// dry-run applied as well.
///
/// Objects in a namespace that the manifest itself creates can't be dry-run before the namespace
/// exists, so those are skipped.
pub(super) async fn dry_run(
    manifest: &Manifest,
    apis: &[Api<DynamicObject>],
) -> Result<(), OperatorInstallError> {
    let create_params = PostParams {
        dry_run: true,
        field_manager: Some(FIELD_MANAGER.to_owned()),
    };
    let apply_params = PatchParams::apply(FIELD_MANAGER).dry_run();

    let created_namespaces = manifest
        .objects()
        .iter()
        .filter(|object| kind(object) == "Namespace")
        .map(ResourceExt::name_any)
        .collect::<HashSet<_>>();

    let mut leftovers = Vec::new();
    let mut foreign = Vec::new();
    for (object, api) in manifest.objects().iter().zip(apis) {
        match api.create(&create_params, object).await {
            Ok(_) => {
                api.patch(&object.name_any(), &apply_params, &Patch::Apply(object))
                    .await
                    .map_err(|source| OperatorInstallError::Rejected {
                        object: describe(object),
                        source: Box::new(source),
                    })?;
            }
            Err(kube::Error::Api(status)) if status.code == StatusCode::CONFLICT => {
                let existing = api
                    .get_metadata(&object.name_any())
                    .await
                    .map_err(|source| OperatorInstallError::Lookup {
                        object: describe(object),
                        source: Box::new(source),
                    })?;

                if manifest::is_attributed_to_release(&existing) {
                    leftovers.push(describe(object));
                } else if kind(object) == "Namespace" {
                    // A namespace that the user made can hold other workloads, and only the user
                    // can decide to remove it.
                    return Err(OperatorInstallError::ExistingNamespace {
                        namespace: object.name_any(),
                    });
                } else {
                    foreign.push(describe(object));
                }
            }
            Err(kube::Error::Api(status))
                if status.code == StatusCode::NOT_FOUND
                    && api
                        .namespace()
                        .is_some_and(|namespace| created_namespaces.contains(namespace)) => {}
            Err(error) => {
                return Err(OperatorInstallError::Rejected {
                    object: describe(object),
                    source: Box::new(error),
                });
            }
        }
    }

    // Reported first, since the user must remove them in a different way than the leftovers.
    if foreign.is_empty().not() {
        Err(OperatorInstallError::ForeignObjects { objects: foreign })
    } else if leftovers.is_empty().not() {
        Err(OperatorInstallError::LeftoverObjects { objects: leftovers })
    } else {
        Ok(())
    }
}

/// Creates the objects in manifest order, which is the order helm installs them in.
///
/// Uses server-side apply the way helm does, see [`FIELD_MANAGER`]. [`dry_run`] already made sure
/// none of the objects exist.
pub(super) async fn create(
    manifest: &Manifest,
    apis: &[Api<DynamicObject>],
) -> Result<(), OperatorInstallError> {
    let params = PatchParams::apply(FIELD_MANAGER);

    for (object, api) in manifest.objects().iter().zip(apis) {
        api.patch(&object.name_any(), &params, &Patch::Apply(object))
            .await
            .map_err(|source| OperatorInstallError::Rejected {
                object: describe(object),
                source: Box::new(source),
            })?;
    }

    Ok(())
}

/// Waits until the operator serves its status, which requires its pod to be up and its API to be
/// registered. `context_arg` gives the command in the error the kubecontext of the run.
pub(super) async fn wait_for_operator(
    client: &Client,
    namespace: &str,
    context_arg: &str,
) -> Result<MirrordOperatorCrd, OperatorInstallError> {
    let api = Api::<MirrordOperatorCrd>::all(client.clone());
    let deadline = Instant::now() + READY_TIMEOUT;

    loop {
        match api.get(OPERATOR_STATUS_NAME).await {
            Ok(operator) => return Ok(operator),
            Err(error) if Instant::now() >= deadline => {
                return Err(OperatorInstallError::NotReady {
                    namespace: namespace.to_owned(),
                    context_arg: context_arg.to_owned(),
                    timeout: READY_TIMEOUT,
                    source: Box::new(error),
                });
            }
            Err(_) => tokio::time::sleep(READY_POLL_INTERVAL).await,
        }
    }
}

fn kind(object: &DynamicObject) -> &str {
    object
        .types
        .as_ref()
        .map(|types| types.kind.as_str())
        .unwrap_or_default()
}

fn describe(object: &DynamicObject) -> String {
    format!("{} `{}`", kind(object), object.name_any())
}
