//! `mirrord operator uninstall`: removes what `mirrord operator install` installed, also when the
//! installation failed half-way.
//!
//! Removes the operator the way `helm uninstall` removes the chart: first the chart's `pre-delete`
//! hook (`helm-cleanup` in the operator image) lets the operator finalize its sessions, then the
//! objects of the manifest are deleted.

use std::{collections::HashSet, ops::Not, time::Duration};

use futures::future::try_join_all;
use http::StatusCode;
use k8s_openapi::{
    api::{apps::v1::Deployment, core::v1::Secret},
    apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition,
};
use kube::{
    Api, Client, ResourceExt,
    api::{ApiResource, DeleteParams, DynamicObject, ListParams, Patch, PatchParams},
    core::GroupVersionKind,
};
use mirrord_operator::crd::{MirrordOperatorCrd, OPERATOR_STATUS_NAME};
use mirrord_progress::{Progress, ProgressTracker};
use tokio::time::Instant;

use super::{
    Connection, OperatorInstallError, cluster, confirm, context_flag, location,
    manifest::{self, Manifest},
};
use crate::config::OperatorUninstallArgs;

/// The CRDs of the objects that the operator finalizes, in the order that the chart's `pre-delete`
/// hook removes them in. The sessions go first, so that the operator reverts what they changed
/// (e.g. workloads that it scaled down), then the patch requests that the sessions made, and then
/// the patches and resources that are left.
///
/// Deleting a CRD with objects that still have finalizers blocks until the finalizers are gone,
/// which only the operator, or [`clear_finalized`] after a timeout, removes.
///
/// Keep this list in sync with the hook (`crates/operator-hooks/src/cleanup.rs` in the operator
/// repository): a new CRD whose objects the operator finalizes must go in both.
const FINALIZED_CRDS: [&[&str]; 3] = [
    &[
        "mirrordclustersessions.mirrord.metalbear.co",
        "mirrordmulticlustersessions.operator.metalbear.co",
        "previewsessions.preview.mirrord.metalbear.co",
        "branchdatabases.dbs.mirrord.metalbear.co",
    ],
    &["mirrordclusterworkloadpatchrequests.mirrord.metalbear.co"],
    &[
        "mirrordclusterworkloadpatches.mirrord.metalbear.co",
        "mirrordclusterexternalresources.mirrord.metalbear.co",
    ],
];

/// How long a running operator gets to finalize deleted objects before their finalizers are
/// removed without it. Longer than the 60 seconds that the operator keeps a deleted session.
const STRIP_FINALIZERS_AFTER: Duration = Duration::from_secs(75);

const CLEAR_TIMEOUT: Duration = Duration::from_mins(2);

const REMOVAL_TIMEOUT: Duration = Duration::from_mins(5);

const POLL_INTERVAL: Duration = Duration::from_secs(1);

pub(in crate::operator) async fn operator_uninstall(
    args: OperatorUninstallArgs,
) -> Result<(), OperatorInstallError> {
    let OperatorUninstallArgs {
        context,
        yes,
        manifest: manifest_path,
    } = args;

    let mut progress = ProgressTracker::from_env("mirrord operator uninstall");
    let Connection {
        client,
        http,
        release_namespace,
        context,
    } = Connection::new(context).await?;

    let mut subtask = progress.subtask("looking for the operator");
    let manifest = match &manifest_path {
        Some(path) => Manifest::parse(&manifest::read_manifest(path)?)?,
        None => installed_manifest(&client, &http).await?,
    };
    let apis = cluster::resolve_apis(&client, &manifest, &release_namespace).await?;
    let installed = find_installed(&client, &manifest, &apis, context.as_deref()).await?;
    let installation = location(manifest.operator_namespace(), context.as_deref());
    if installed.is_empty() {
        subtask.success(Some("no operator installed"));
        progress.success(None);
        println!("No mirrord operator is installed in {installation}.");
        return Ok(());
    }
    subtask.success(None);

    let question = format!(
        "Remove the mirrord operator from {installation}? This also deletes all mirrord policies \
        and profiles of the cluster."
    );
    confirm(&progress, yes, &question)?;

    let mut subtask = progress.subtask("finalizing operator sessions");
    let installed_crds = installed
        .iter()
        .filter(|(object, _)| cluster::kind(object) == "CustomResourceDefinition")
        .map(|(object, _)| object.name_any())
        .collect();
    finalize_sessions(&client, &installed_crds).await?;
    subtask.success(None);

    let mut subtask = progress.subtask("removing the operator");
    try_join_all(installed.iter().map(|(object, api)| async move {
        ignore_not_found(
            api.delete(&object.name_any(), &DeleteParams::default())
                .await,
        )
        .map_err(|source| OperatorInstallError::Delete {
            object: cluster::describe(object),
            source: Box::new(source),
        })
    }))
    .await?;
    subtask.success(None);

    let mut subtask = progress.subtask("waiting for the objects to be removed");
    wait_for_removal(installed).await?;
    subtask.success(None);
    progress.success(None);

    println!("Removed the mirrord operator from {installation}.");

    Ok(())
}

/// The manifest of the chart version that is installed, which can be older than the latest one
/// and have objects that the latest one does not.
///
/// The latest manifest if no operator Deployment is in the cluster.
async fn installed_manifest(
    client: &Client,
    http: &reqwest::Client,
) -> Result<Manifest, OperatorInstallError> {
    let latest = manifest::latest_chart_version(http).await?;
    let manifest = Manifest::parse(&manifest::fetch_manifest(http, latest.clone()).await?)?;

    let deployment = Api::<Deployment>::namespaced(client.clone(), manifest.operator_namespace())
        .get_opt(manifest.operator_deployment())
        .await
        .map_err(|source| OperatorInstallError::Lookup {
            object: format!("Deployment `{}`", manifest.operator_deployment()),
            source: Box::new(source),
        })?;
    let Some(installed) = deployment
        .as_ref()
        .and_then(manifest::chart_version)
        .filter(|installed| *installed != latest)
    else {
        return Ok(manifest);
    };

    match manifest::fetch_manifest(http, installed).await {
        Ok(installed) => Manifest::parse(&installed),
        // Chart versions from before the manifests were published, which `mirrord operator
        // install` did not install.
        Err(OperatorInstallError::ManifestNotPublished { .. }) => Ok(manifest),
        Err(error) => Err(error),
    }
}

/// The objects of the manifest that are in the cluster, with their APIs.
///
/// Fails if an object does not belong to the operator's helm release, since a different tool
/// created it, or if helm manages the release, since `helm uninstall` must remove it then. A
/// namespace that does not belong to the release is not part of the installation either, but
/// the user can make one without installing the operator, so it is left alone.
async fn find_installed<'a>(
    client: &Client,
    manifest: &'a Manifest,
    apis: &'a [Api<DynamicObject>],
    context: Option<&str>,
) -> Result<Vec<(&'a DynamicObject, &'a Api<DynamicObject>)>, OperatorInstallError> {
    let existing = try_join_all(manifest.objects().iter().zip(apis).map(
        |(object, api)| async move {
            api.get_metadata_opt(&object.name_any())
                .await
                .map(|existing| existing.map(|existing| (object, api, existing)))
                .map_err(|source| OperatorInstallError::Lookup {
                    object: cluster::describe(object),
                    source: Box::new(source),
                })
        },
    ))
    .await?;

    let mut installed = Vec::new();
    let mut foreign = Vec::new();
    let mut release_namespace = None;
    for (object, api, existing) in existing.into_iter().flatten() {
        if manifest::is_attributed_to_release(&existing) {
            release_namespace = release_namespace.or_else(|| {
                existing
                    .annotations()
                    .get(manifest::RELEASE_NAMESPACE_ANNOTATION)
                    .cloned()
            });
            installed.push((object, api));
        } else if cluster::kind(object) != "Namespace" {
            foreign.push(cluster::describe(object));
        }
    }

    if foreign.is_empty().not() {
        return Err(OperatorInstallError::ForeignObjects { objects: foreign });
    }

    // helm stores each revision of a release in a Secret with these labels.
    if let Some(namespace) = release_namespace {
        let releases = Api::<Secret>::namespaced(client.clone(), &namespace)
            .list_metadata(
                &ListParams::default()
                    // `helm uninstall --keep-history` keeps the revisions with this status.
                    .labels(&format!(
                        "owner=helm,name={},status!=uninstalled",
                        manifest::RELEASE_NAME
                    )),
            )
            .await
            .map_err(|source| OperatorInstallError::Lookup {
                object: format!("the helm releases in namespace `{namespace}`"),
                source: Box::new(source),
            })?;
        if releases.items.is_empty().not() {
            return Err(OperatorInstallError::ManagedByHelm {
                uninstall: format!(
                    "helm uninstall {} -n {namespace}{}",
                    manifest::RELEASE_NAME,
                    context_flag("--kube-context", context),
                ),
            });
        }
    }

    Ok(installed)
}

/// Lets the operator finalize its objects, one group of [`FINALIZED_CRDS`] after the other, as
/// the chart's `pre-delete` hook does.
///
/// [`FINALIZED_CRDS`] also has CRDs of optional chart features, which a default installation does
/// not have. Only the CRDs in `installed_crds`, which uninstall deletes, are cleared.
async fn finalize_sessions(
    client: &Client,
    installed_crds: &HashSet<String>,
) -> Result<(), OperatorInstallError> {
    for crds in FINALIZED_CRDS {
        let crds = crds
            .iter()
            .copied()
            .filter(|crd| installed_crds.contains(*crd))
            .collect::<Vec<_>>();
        if crds.is_empty() {
            continue;
        }

        // When the operator API is gone (404) or does not respond (503), nothing removes the
        // finalizers, so there is no point in waiting. Other errors, e.g. of the network, say
        // nothing about the operator, so it still gets the time.
        let strip_finalizers_after = match Api::<MirrordOperatorCrd>::all(client.clone())
            .get(OPERATOR_STATUS_NAME)
            .await
        {
            Err(kube::Error::Api(status))
                if status.code == StatusCode::NOT_FOUND
                    || status.code == StatusCode::SERVICE_UNAVAILABLE =>
            {
                Duration::ZERO
            }
            _ => STRIP_FINALIZERS_AFTER,
        };
        clear_finalized(client, &crds, strip_finalizers_after).await?;
    }

    Ok(())
}

/// Deletes all objects of `crds` and waits until they are gone.
///
/// The operator gets `strip_finalizers_after` to finalize them. After that, their finalizers are
/// removed, so that they do not stay forever and block the removal of their CRDs. Since the
/// operator is removed after this, its unfinished work is lost either way.
async fn clear_finalized(
    client: &Client,
    crds: &[&str],
    strip_finalizers_after: Duration,
) -> Result<(), OperatorInstallError> {
    let mut resources = Vec::with_capacity(crds.len());
    for &name in crds {
        let crd = Api::<CustomResourceDefinition>::all(client.clone())
            .get_opt(name)
            .await
            .map_err(|source| OperatorInstallError::Lookup {
                object: format!("CustomResourceDefinition `{name}`"),
                source: Box::new(source),
            })?;
        // The CRDs of optional features are only in the cluster when the features are enabled.
        let Some(crd) = crd else {
            continue;
        };
        let Some(version) = crd.spec.versions.iter().find(|version| version.storage) else {
            continue;
        };

        let gvk = GroupVersionKind::gvk(&crd.spec.group, &version.name, &crd.spec.names.kind);
        resources.push((
            name,
            ApiResource::from_gvk_with_plural(&gvk, &crd.spec.names.plural),
        ));
    }

    let started = Instant::now();
    loop {
        let mut remaining = Vec::new();
        // A CRD that was created moments before, e.g. by an installation that was stopped right
        // after, answers with 429 until its storage is ready. Its objects are listed again on the
        // next pass.
        let mut unready = None;
        for (crd, resource) in &resources {
            match Api::<DynamicObject>::all_with(client.clone(), resource)
                .list_metadata(&ListParams::default())
                .await
            {
                Ok(objects) => {
                    remaining.extend(objects.items.into_iter().map(|object| (resource, object)))
                }
                Err(error)
                    if matches!(
                        &error,
                        kube::Error::Api(status) if status.code == StatusCode::TOO_MANY_REQUESTS
                    ) =>
                {
                    unready = Some((crd, error))
                }
                Err(source) => {
                    return Err(OperatorInstallError::Lookup {
                        object: format!("the objects of CustomResourceDefinition `{crd}`"),
                        source: Box::new(source),
                    });
                }
            }
        }

        let timed_out = started.elapsed() >= CLEAR_TIMEOUT;
        match unready {
            None if remaining.is_empty() => return Ok(()),
            Some((crd, source)) if timed_out && remaining.is_empty() => {
                return Err(OperatorInstallError::Lookup {
                    object: format!("the objects of CustomResourceDefinition `{crd}`"),
                    source: Box::new(source),
                });
            }
            _ if timed_out => {
                return Err(OperatorInstallError::NotFinalized {
                    remaining: remaining.len(),
                });
            }
            _ => {}
        }

        let strip_finalizers = started.elapsed() >= strip_finalizers_after;
        try_join_all(remaining.into_iter().map(|(resource, object)| async move {
            let api: Api<DynamicObject> = match object.namespace() {
                Some(namespace) => Api::namespaced_with(client.clone(), &namespace, resource),
                None => Api::all_with(client.clone(), resource),
            };
            let name = object.name_any();
            let describe = || format!("{} `{name}`", resource.kind);

            if object.metadata.deletion_timestamp.is_none() {
                ignore_not_found(api.delete(&name, &DeleteParams::default()).await).map_err(
                    |source| OperatorInstallError::Delete {
                        object: describe(),
                        source: Box::new(source),
                    },
                )
            } else if strip_finalizers && object.finalizers().is_empty().not() {
                let patch = serde_json::json!({ "metadata": { "finalizers": null } });
                ignore_not_found(
                    api.patch_metadata(&name, &PatchParams::default(), &Patch::Merge(&patch))
                        .await,
                )
                .map_err(|source| OperatorInstallError::RemoveFinalizers {
                    object: describe(),
                    source: Box::new(source),
                })
            } else {
                Ok(())
            }
        }))
        .await?;

        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Maps a 404 to success, since the goal of each request here is that the object is gone.
fn ignore_not_found<T>(result: kube::Result<T>) -> kube::Result<()> {
    match result {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(status)) if status.code == StatusCode::NOT_FOUND => Ok(()),
        Err(error) => Err(error),
    }
}

/// Waits until all deleted objects are gone, so that installing the operator again right after
/// does not find them, e.g. the namespace while it is still terminating.
async fn wait_for_removal(
    mut remaining: Vec<(&DynamicObject, &Api<DynamicObject>)>,
) -> Result<(), OperatorInstallError> {
    let deadline = Instant::now() + REMOVAL_TIMEOUT;

    loop {
        remaining = try_join_all(remaining.into_iter().map(|(object, api)| async move {
            let existing = api
                .get_metadata_opt(&object.name_any())
                .await
                .map_err(|source| OperatorInstallError::Lookup {
                    object: cluster::describe(object),
                    source: Box::new(source),
                })?;
            Ok::<_, OperatorInstallError>(existing.map(|_| (object, api)))
        }))
        .await?
        .into_iter()
        .flatten()
        .collect();

        if remaining.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(OperatorInstallError::NotRemoved {
                objects: remaining
                    .iter()
                    .map(|(object, _)| cluster::describe(object))
                    .collect(),
                timeout: REMOVAL_TIMEOUT,
            });
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
