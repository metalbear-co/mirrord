//! What `mirrord up` checks before it starts the services that request DB branching
//! (`feature.db_branches: "*"`, the default of the modes that branch). The operator is asked what
//! each service's request resolves to, so an operator that can't serve it, or rejects it, stops
//! the run before any session starts, and what the services will get is printed up front.
//!
//! The sessions still resolve the request themselves: the lookup here creates nothing, and each
//! session's resolution is what marks the branches as shared under the run's key.

use std::{
    collections::{BTreeMap, HashMap},
    ops::Not,
    sync::Arc,
};

use mirrord_analytics::NullReporter;
use mirrord_config::{
    LayerConfig,
    feature::database_branches::{BranchCopyMode, DatabaseBranchesConfig},
};
use mirrord_operator::client::{
    NoClientCert, OperatorApi, error::OperatorApiError,
    split_config_branches::entries_from_response,
};
use mirrord_progress::NullProgress;
use mirrord_up::{ServiceMode, SubprocessCfg};

use super::UpCliError;

/// Asks the operator what each service requesting DB branching gets, printing it, before any
/// service starts.
///
/// Services whose cluster has no usable operator (not installed, disabled, or with an expired
/// license) run without branching, unless the config insists on the operator, in which case
/// their session reports it. A service that gets no entries back runs without branching too.
///
/// Returns the services that requested DB branching, including those that got no entries.
pub(super) async fn check_db_branches(
    services: &mut [SubprocessCfg],
) -> Result<Vec<Arc<str>>, UpCliError> {
    let mut apis: HashMap<Option<String>, Option<OperatorApi<NoClientCert>>> = HashMap::new();
    for service in services.iter_mut() {
        if service
            .config
            .feature
            .db_branches
            .split_config_request()
            .is_none()
        {
            continue;
        }
        let context = service.config.kube_context.clone();
        if apis.contains_key(&context).not() {
            let api =
                usable_operator(&service.config)
                    .await
                    .map_err(|error| UpCliError::DbBranches {
                        service: service.service_name.clone(),
                        error: Box::new(error),
                    })?;
            apis.insert(context.clone(), api);
        }
        if apis.get(&context).is_some_and(Option::is_none) && service.config.operator != Some(true)
        {
            service.config.feature.db_branches = DatabaseBranchesConfig::default();
        }
    }

    let requesting = services
        .iter_mut()
        .filter(|service| {
            service
                .config
                .feature
                .db_branches
                .split_config_request()
                .is_some()
        })
        .collect::<Vec<_>>();
    let requested = requesting
        .iter()
        .map(|service| service.service_name.clone())
        .collect::<Vec<_>>();
    for service in &requested {
        println!("{service}: requesting DB branching");
    }

    // Entry id -> the namespaces it resolved in, and the services in each.
    let mut entry_namespaces: BTreeMap<String, BTreeMap<String, Vec<Arc<str>>>> = BTreeMap::new();
    for service in requesting {
        let Some(Some(api)) = apis.get(&service.config.kube_context) else {
            continue;
        };
        let lookup = async {
            let response = api.lookup_split_config_db_branches(&service.config).await?;
            let resolved = response
                .as_ref()
                .map(|response| entries_from_response(response, service.config.key.as_str()))
                .transpose()?;
            Ok::<_, OperatorApiError>((response, resolved))
        };
        let (response, resolved) = lookup.await.map_err(|error| UpCliError::DbBranches {
            service: service.service_name.clone(),
            error: Box::new(error),
        })?;
        let (Some(response), Some(resolved)) = (response, resolved) else {
            continue;
        };

        if response.entries.is_empty() {
            println!(
                "{}: no DB branch definitions, running against the source DB",
                service.service_name
            );
            service.config.feature.db_branches = DatabaseBranchesConfig::default();
            continue;
        }

        let namespace = service
            .config
            .target
            .namespace
            .as_deref()
            .unwrap_or(api.client().default_namespace())
            .to_owned();
        for (entry, config) in response.entries.iter().zip(&resolved.entries) {
            entry_namespaces
                .entry(entry.id.clone())
                .or_default()
                .entry(namespace.clone())
                .or_default()
                .push(service.service_name.clone());

            // Mirrored traffic is also served by the cluster, against the source rows, so a
            // branch without them answers differently from the original.
            if service.mode == ServiceMode::Mirror
                && let Some(mode @ (BranchCopyMode::Empty | BranchCopyMode::Schema)) =
                    config.copy_mode()
            {
                println!(
                    "{}: branch `{}` copies {}, so it has no rows; the mirrored requests the \
                     service handles read none of the source data",
                    service.service_name,
                    entry.id,
                    if mode == BranchCopyMode::Empty {
                        "nothing"
                    } else {
                        "only the schema"
                    }
                );
            }
        }
    }

    for (entry, namespaces) in entry_namespaces {
        if namespaces.len() < 2 {
            continue;
        }
        let services_by_namespace = namespaces
            .iter()
            .map(|(namespace, services)| format!("{} in `{namespace}`", services.join(", ")))
            .collect::<Vec<_>>()
            .join("; ");
        println!(
            "DB branch `{entry}` is requested from more than one namespace ({services_by_namespace}); \
             each namespace gets its own branch, so those branches are not shared"
        );
    }

    Ok(requested)
}

/// The operator a session with `config` would use for branching, if any. Mirrors how the
/// sessions decide to use the operator, so a service only requests branching when its session
/// will be able to.
async fn usable_operator(
    config: &LayerConfig,
) -> Result<Option<OperatorApi<NoClientCert>>, OperatorApiError> {
    if config.operator == Some(false) {
        return Ok(None);
    }
    let Some(api) =
        OperatorApi::try_new(config, &mut NullReporter::default(), &NullProgress).await?
    else {
        return Ok(None);
    };
    Ok(api
        .check_license_validity(&NullProgress)
        .is_ok()
        .then_some(api))
}
