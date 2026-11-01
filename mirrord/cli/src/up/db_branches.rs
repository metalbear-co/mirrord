//! What `mirrord up` checks before it starts the services that request DB branching
//! (`feature.db_branches: "*"`, the default of the modes that branch). The operator is asked what
//! each service's request resolves to, so an operator that can't serve it, or rejects it, stops
//! the run before any session starts, and what the services will get is printed up front.
//!
//! The sessions still resolve the request themselves: the lookup here creates nothing, and each
//! session's resolution is what marks the branches as shared under the run's key.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    ops::Not,
    sync::Arc,
};

use futures::future::join_all;
use mirrord_analytics::NullReporter;
use mirrord_config::{LayerConfig, feature::database_branches::DatabaseBranchesConfig};
use mirrord_operator::{
    client::{
        NoClientCert, OperatorApi, error::OperatorApiError,
        split_config_branches::apply_resolved_entries,
    },
    crd::NewOperatorFeature,
};
use mirrord_progress::NullProgress;
use mirrord_up::SubprocessCfg;

use super::UpCliError;
use crate::{
    connection::{OperatorAvailability, operator_availability, report_local_feature_gate},
    error::CliError,
};

/// What a service is told when [`resolves_default_db_branches`] says no and its default DB
/// branching is dropped.
pub(crate) const DEFAULT_DB_BRANCHES_UNSUPPORTED: &str =
    "the mirrord operator doesn't support DB branching, running against the source DB";

/// Whether `operator` can resolve the `"*"` that `mirrord up`, `mirrord preview up` and `mirrord
/// preview sync` add to a service's DB branches by default.
///
/// The operator leaves the feature out when its branching is off, its license tier doesn't
/// include it, or it predates it. A `"*"` the user didn't ask for must not stop the service on
/// such an operator, so the callers drop it instead.
pub(crate) fn resolves_default_db_branches(operator: &OperatorApi<NoClientCert>) -> bool {
    operator
        .operator()
        .spec
        .supported_features()
        .contains(&NewOperatorFeature::DbBranchesFromSplitConfig)
}

/// Asks the operator what each service requesting DB branching gets, printing it, before any
/// service starts.
///
/// Services whose cluster has no usable operator (not installed, disabled, or with an expired
/// license) run without branching, unless the config insists on the operator (`operator: true`),
/// in which case the run stops with the error the session would give. So do services with the
/// default `"*"` whose operator can't resolve it, and services that get no entries back.
///
/// Services that define their branches inline (e.g. through `config_patch`) have nothing to look
/// up; they are only announced.
///
/// Returns the services that requested DB branching: those asking the operator, including those
/// that got no entries, and those with inline branches.
pub(super) async fn check_db_branches(
    services: &mut [SubprocessCfg],
) -> Result<Vec<Arc<str>>, UpCliError> {
    let mut apis: HashMap<OperatorKey, Option<OperatorApi<NoClientCert>>> = HashMap::new();
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
        let key = operator_key(&service.config);
        if apis.contains_key(&key).not() {
            let api =
                usable_operator(&service.config)
                    .await
                    .map_err(|error| UpCliError::DbBranches {
                        service: service.service_name.clone(),
                        error: Box::new(error),
                    })?;
            apis.insert(key.clone(), api);
        }
        match apis.get(&key) {
            Some(None) => {
                println!(
                    "{}: no mirrord operator, running against the source DB",
                    service.service_name
                );
                service.config.feature.db_branches = DatabaseBranchesConfig::default();
            }
            // One the user set fails below, as it would in a session.
            Some(Some(api))
                if service.default_db_branches && resolves_default_db_branches(api).not() =>
            {
                println!(
                    "{}: {DEFAULT_DB_BRANCHES_UNSUPPORTED}",
                    service.service_name
                );
                service.config.feature.db_branches = DatabaseBranchesConfig::default();
            }
            _ => {}
        }
    }

    let inline = services
        .iter()
        .filter(|service| service.config.feature.db_branches.inline().is_empty().not())
        .map(|service| service.service_name.clone())
        .collect::<Vec<_>>();
    // Every service still asking, with its request and the operator to ask: a required operator
    // that can't be used has failed the run above, and a missing optional one dropped the request.
    let requesting = services
        .iter_mut()
        .filter_map(|service| {
            let request = service
                .config
                .feature
                .db_branches
                .split_config_request()?
                .clone();
            let key = operator_key(&service.config);
            apis.get(&key)?.as_ref()?;
            Some((service, request, key))
        })
        .collect::<Vec<_>>();
    let requested = requesting
        .iter()
        .map(|(service, ..)| service.service_name.clone())
        .collect::<Vec<_>>();
    for service in &requested {
        println!("{service}: requesting DB branching");
    }
    for service in &inline {
        println!("{service}: using its own DB branch definitions");
    }

    // The lookups are independent, so they run at once: the run waits about one round trip to
    // the operator instead of one per service. `join_all` keeps the results in service order, so
    // the failure reported below is still the first failing service's.
    let lookups = join_all(requesting.iter().map(|(service, request, key)| {
        let api = apis
            .get(key)
            .and_then(Option::as_ref)
            .expect("requesting services have a usable operator");
        async move {
            let response = api
                .lookup_split_config_db_branches(&service.config, request)
                .await?;
            // The session applies the answer to its config the same way. Applied to a copy
            // here, it rejects what the session would (e.g. an env override clashing with a
            // branch's connection variable), before any session starts.
            apply_resolved_entries(&mut service.config.clone(), &response)?;
            Ok::<_, OperatorApiError>(response)
        }
    }))
    .await;

    // Entry id -> the (cluster, namespace) pairs it resolved in, and the services in each. A
    // branch is named for its namespace and lives in its cluster, so each pair is one branch. The
    // cluster is the API server URL, since two kube contexts can name one cluster.
    let mut entry_namespaces: BTreeMap<String, BTreeMap<_, Vec<Arc<str>>>> = BTreeMap::new();
    for ((service, _, key), lookup) in requesting.into_iter().zip(lookups) {
        let response = match lookup {
            Ok(response) => response,
            Err(error) => {
                if service.default_db_branches.not()
                    && let OperatorApiError::LocalFeatureNotLicensed { feature, .. } = &error
                {
                    // Lookups share a Kubernetes-only client. Only the refusal that stops the
                    // run needs credentials for reporting; no resource action follows it.
                    let api = apis
                        .remove(&key)
                        .flatten()
                        .expect("requesting services have a usable operator");
                    report_local_feature_gate(
                        api,
                        *feature,
                        &service.config,
                        &NullProgress,
                        &mut NullReporter::default(),
                    )
                    .await;
                }
                return Err(UpCliError::DbBranches {
                    service: service.service_name.clone(),
                    error: Box::new(error.into()),
                });
            }
        };

        if response.entries.is_empty() {
            println!(
                "{}: no DB branch definitions, running against the source DB",
                service.service_name
            );
            service.config.feature.db_branches = DatabaseBranchesConfig::default();
            continue;
        }

        let api = apis
            .get(&key)
            .and_then(Option::as_ref)
            .expect("requesting services have a usable operator");
        let namespace = service
            .config
            .target
            .namespace
            .as_deref()
            .unwrap_or(api.client().default_namespace())
            .to_owned();
        for entry in &response.entries {
            entry_namespaces
                .entry(entry.id.clone())
                .or_default()
                .entry((api.cluster_url().to_owned(), namespace.clone()))
                .or_default()
                .push(service.service_name.clone());
        }
    }

    for (entry, namespaces) in entry_namespaces {
        if namespaces.len() < 2 {
            continue;
        }
        // The cluster only tells the pairs apart when they span more than one.
        let several_clusters = namespaces
            .keys()
            .map(|(cluster, _)| cluster)
            .collect::<BTreeSet<_>>()
            .len()
            > 1;
        let services_by_namespace = namespaces
            .iter()
            .map(|((cluster, namespace), services)| {
                if several_clusters {
                    format!(
                        "{} in `{namespace}` of cluster `{cluster}`",
                        services.join(", ")
                    )
                } else {
                    format!("{} in `{namespace}`", services.join(", "))
                }
            })
            .collect::<Vec<_>>()
            .join("; ");
        println!(
            "DB branch `{entry}` is requested from more than one namespace ({services_by_namespace}); \
             each namespace gets its own branch, so those branches are not shared"
        );
    }

    Ok(requested.into_iter().chain(inline).collect())
}

/// The settings that decide which operator a session finds and whether it has to use it, so
/// services that agree on them share one [`usable_operator`] answer.
type OperatorKey = (Option<String>, Option<String>, Option<bool>, Option<bool>);

fn operator_key(config: &LayerConfig) -> OperatorKey {
    (
        config.kubeconfig.clone(),
        config.kube_context.clone(),
        config.accept_invalid_certificates,
        config.operator,
    )
}

/// The operator a session with `config` would use for branching, if any, decided as the session
/// decides it ([`operator_availability`]). So a service only requests branching when its session
/// will be able to, and an operator the config requires but can't be used fails here, before any
/// session starts, with the session's error.
async fn usable_operator(
    config: &LayerConfig,
) -> Result<Option<OperatorApi<NoClientCert>>, CliError> {
    let required = config.operator == Some(true);
    match operator_availability(
        config,
        &mut NullReporter::default(),
        &NullProgress,
        &NullProgress,
    )
    .await?
    {
        OperatorAvailability::Usable(api) => Ok(Some(*api)),
        OperatorAvailability::NotInstalled if required => Err(CliError::OperatorNotInstalled),
        OperatorAvailability::LicenseInvalid(error) if required => Err(error.into()),
        OperatorAvailability::Disabled
        | OperatorAvailability::NotInstalled
        | OperatorAvailability::LicenseInvalid(_) => Ok(None),
    }
}
