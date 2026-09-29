//! Cluster side of the installation: checking for an existing operator, creating the manifest's
//! objects, and waiting for the operator to come up.

use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    time::Duration,
};

use http::StatusCode;
use k8s_openapi::kube_aggregator::pkg::apis::apiregistration::v1::APIService;
use kube::{
    Api, Client, ResourceExt,
    api::{DynamicObject, Patch, PatchParams, PostParams},
    core::GroupVersionKind,
    discovery::{self, Scope},
};
use mirrord_operator::crd::{MirrordOperatorCrd, OPERATOR_STATUS_NAME};
use tokio::time::Instant;

use super::{error::OperatorInstallError, manifest::Manifest};

/// Registers the operator API with the Kubernetes API server, present as long as an operator is
/// installed.
const OPERATOR_API_SERVICE: &str = "v1.operator.metalbear.co";

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
/// healthy or not.
pub(super) async fn ensure_no_operator(client: &Client) -> Result<(), OperatorInstallError> {
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
            source: Box::new(error),
        }),
    }
}

/// Dry-runs creating every object, so missing permissions and leftovers of an earlier
/// installation surface before anything is created or a trial is started.
///
/// Objects in a namespace that the manifest itself creates can't be dry-run before the namespace
/// exists, so those are skipped.
pub(super) async fn dry_run(
    manifest: &Manifest,
    apis: &[Api<DynamicObject>],
) -> Result<(), OperatorInstallError> {
    let params = PostParams {
        dry_run: true,
        field_manager: Some(FIELD_MANAGER.to_owned()),
    };

    let created_namespaces = manifest
        .objects()
        .iter()
        .filter(|object| kind(object) == "Namespace")
        .map(ResourceExt::name_any)
        .collect::<HashSet<_>>();

    let mut leftovers = Vec::new();
    for (object, api) in manifest.objects().iter().zip(apis) {
        match api.create(&params, object).await {
            Ok(_) => {}
            Err(kube::Error::Api(status)) if status.code == StatusCode::CONFLICT => {
                leftovers.push(describe(object))
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

    if leftovers.is_empty() {
        Ok(())
    } else {
        Err(OperatorInstallError::LeftoverObjects { objects: leftovers })
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
