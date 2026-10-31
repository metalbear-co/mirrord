//! What `mirrord up` checks before it starts the services that request DB branching
//! (`feature.db_branches: "*"`, the default of the modes that branch) or split queues. The
//! operator is asked what each service's DB branching request resolves to, so an operator that
//! can't serve it, or rejects it, stops the run before any session starts, and what the services
//! will get is printed up front. A mode's `queues` line the operator can't serve stops it too.
//!
//! The sessions still resolve the request themselves: the lookup here creates nothing, and each
//! session's resolution is what marks the branches as shared under the run's key.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    ops::Not,
    sync::Arc,
};

use futures::future::join_all;
use itertools::Itertools;
use mirrord_analytics::NullReporter;
use mirrord_config::{
    LayerConfig,
    feature::database_branches::{
        DatabaseBranchConfig, DatabaseBranchEngine, DatabaseBranchesConfig,
    },
};
use mirrord_operator::{
    client::{
        NoClientCert, OperatorApi, error::OperatorApiError, required_branching_feature,
        split_config_branches::apply_resolved_entries,
    },
    crd::NewOperatorFeature,
};
use mirrord_progress::NullProgress;
use mirrord_up::{RequestOrigin, SubprocessCfg};

use super::UpCliError;
use crate::{
    connection::{OperatorAvailability, operator_availability, report_local_feature_gate},
    error::CliError,
};

/// The operator each service would use, by the settings that pick it ([`OperatorKey`]), for the
/// services that request DB branching or split queues, or why its cluster has no usable one.
pub(super) type Operators = HashMap<OperatorKey, Result<OperatorApi<NoClientCert>, NoOperator>>;

/// Why a service has no operator to use, for the messages that say so. The license error itself
/// isn't kept: `mirrord operator status` shows it.
#[derive(Clone, Copy, Debug)]
pub(crate) enum NoOperator {
    Disabled,
    NotInstalled,
    LicenseInvalid,
}

impl std::fmt::Display for NoOperator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Disabled => "`operator: false` is set",
            Self::NotInstalled => "none is installed in the cluster",
            Self::LicenseInvalid => {
                "its license can't be used, see `mirrord operator status` for why"
            }
        })
    }
}

/// Looks up the operator of every service that requests DB branching or splits queues, once per
/// [`OperatorKey`]. An operator the config requires (`operator: true`) but that can't be used
/// stops the run with the error the session would give.
pub(super) async fn usable_operators(services: &[SubprocessCfg]) -> Result<Operators, UpCliError> {
    let mut operators = Operators::new();
    for service in services.iter().filter(|service| {
        service
            .config
            .feature
            .db_branches
            .split_config_request()
            .is_some()
            || service.queues_origin != RequestOrigin::Config
    }) {
        let key = operator_key(&service.config);
        if operators.contains_key(&key) {
            continue;
        }
        let operator =
            usable_operator(&service.config)
                .await
                .map_err(|error| UpCliError::DbBranches {
                    service: service.service_name.clone(),
                    error: Box::new(error),
                })?;
        operators.insert(key, operator);
    }
    Ok(operators)
}

/// Stops the run when a mode's `queues` line can't be served: the service has no usable operator,
/// or one that splits none of its queue kinds. Without the line, the run goes on, saying so when
/// the operator splits none of them. With no operator at all it says nothing, as a session
/// wouldn't.
///
/// The queues are split from a wildcard over every broker, less those a Kafka branch takes over,
/// so an operator that splits one of the remaining brokers serves it.
pub(super) fn check_queue_splits(
    services: &[SubprocessCfg],
    operators: &Operators,
) -> Result<(), UpCliError> {
    for service in services {
        let Some(operator) = operators.get(&operator_key(&service.config)) else {
            continue;
        };
        let splits_queues = operator.as_ref().is_ok_and(|api| {
            let supported = api.operator().spec.supported_features();
            service
                .config
                .feature
                .split_queues
                .kinds()
                .into_iter()
                .filter_map(NewOperatorFeature::queue_splitting)
                .any(|feature| supported.contains(&feature))
        });
        match (service.queues_origin, operator) {
            (RequestOrigin::ModeLine, Err(reason)) => {
                return Err(UpCliError::ModeQueuesNoOperator {
                    service: service.service_name.clone(),
                    mode: service.mode.clone(),
                    reason: *reason,
                });
            }
            (RequestOrigin::ModeLine, Ok(_)) if splits_queues.not() => {
                return Err(UpCliError::ModeQueuesUnsupported {
                    service: service.service_name.clone(),
                    mode: service.mode.clone(),
                });
            }
            (RequestOrigin::Default, Ok(_)) if splits_queues.not() => {
                println!(
                    "{}: the mirrord operator doesn't split queues, running without queue \
                     splitting",
                    service.service_name
                );
            }
            _ => {}
        }
    }
    Ok(())
}

/// What a service is told when [`resolves_default_db_branches`] says no and its default DB
/// branching is dropped.
pub(crate) const DEFAULT_DB_BRANCHES_UNSUPPORTED: &str =
    "the mirrord operator doesn't support DB branching, running against the source DB";

/// Whether `operator` can resolve the `"*"` `mirrord up` and `mirrord preview up` add to a
/// service's DB branches by default.
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
/// A request from the mode's `db_branching` line is never dropped: the user asked for branches,
/// so running against the source databases instead would be a surprise. No usable operator, no
/// entries, or a copy mode an entry doesn't allow stops the run, naming the mode to change.
///
/// Services that define their branches inline (e.g. through `config_patch`) have nothing to look
/// up; they are only announced.
///
/// Returns the services that requested DB branching: those asking the operator, including those
/// that got no entries, and those with inline branches.
pub(super) async fn check_db_branches(
    services: &mut [SubprocessCfg],
    apis: &mut Operators,
) -> Result<Vec<Arc<str>>, UpCliError> {
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
        match apis.get(&key) {
            Some(Err(reason)) if service.db_branches_origin == RequestOrigin::ModeLine => {
                return Err(UpCliError::ModeDbBranchesNoOperator {
                    service: service.service_name.clone(),
                    mode: service.mode.clone(),
                    reason: *reason,
                });
            }
            Some(Err(reason)) => {
                println!(
                    "{}: no usable mirrord operator ({reason}), running against the source DB",
                    service.service_name
                );
                service.config.feature.db_branches = DatabaseBranchesConfig::default();
            }
            // One the user set fails below, as it would in a session.
            Some(Ok(api))
                if service.db_branches_origin == RequestOrigin::Default
                    && resolves_default_db_branches(api).not() =>
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
            apis.get(&key)?.as_ref().ok()?;
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
            .and_then(|operator| operator.as_ref().ok())
            .expect("requesting services have a usable operator");
        async move {
            let response = api
                .lookup_split_config_db_branches(&service.config, request)
                .await?;
            // The session applies the answer to its config the same way. Applied to a copy
            // here, it rejects what the session would (e.g. an env override clashing with a
            // branch's connection variable), before any session starts.
            let (resolved, _) = apply_resolved_entries(&mut service.config.clone(), &response)?;
            Ok::<_, OperatorApiError>((response, resolved))
        }
    }))
    .await;

    // Entry id -> the (cluster, namespace) pairs it resolved in, and the services in each. A
    // branch is named for its namespace and lives in its cluster, so each pair is one branch. The
    // cluster is the API server URL, since two kube contexts can name one cluster.
    let mut entry_namespaces: BTreeMap<String, BTreeMap<_, Vec<Arc<str>>>> = BTreeMap::new();
    // Each branch (cluster, namespace, engine, branch id) and the copy mode every service asks it
    // for. Branch names carry the engine, so entries of two engines under one id are two branches.
    let mut branch_modes: BTreeMap<_, Vec<(Arc<str>, String)>> = BTreeMap::new();
    for ((service, _, key), lookup) in requesting.into_iter().zip(lookups) {
        let (response, resolved) = match lookup {
            Ok(lookup) => lookup,
            Err(OperatorApiError::SplitConfigCopyModeNotAllowed(message))
                if service.db_branches_origin == RequestOrigin::ModeLine =>
            {
                return Err(UpCliError::ModeCopyModeNotAllowed {
                    service: service.service_name.clone(),
                    mode: service.mode.clone(),
                    message,
                });
            }
            Err(error) => {
                if service.db_branches_origin != RequestOrigin::Default
                    && let OperatorApiError::LocalFeatureNotLicensed { feature, .. } = &error
                {
                    // Lookups share a Kubernetes-only client. Only the refusal that stops the
                    // run needs credentials for reporting; no resource action follows it.
                    let api = apis
                        .remove(&key)
                        .and_then(Result::ok)
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

        if response.entries.is_empty() && service.db_branches_origin == RequestOrigin::ModeLine {
            return Err(UpCliError::ModeDbBranchesNoDefinitions {
                service: service.service_name.clone(),
                mode: service.mode.clone(),
            });
        }
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
            .and_then(|operator| operator.as_ref().ok())
            .expect("requesting services have a usable operator");

        // The session refuses an entry whose engine the operator has turned off, once the run
        // has started; checked here, a request nobody asked for is dropped instead.
        let supported = api.operator().spec.supported_features();
        let disabled = resolved
            .entries
            .iter()
            .filter_map(required_branching_feature)
            .find(|feature| supported.contains(feature).not());
        match (disabled, service.db_branches_origin) {
            (Some(feature), RequestOrigin::ModeLine) => {
                return Err(UpCliError::ModeDbBranchesFeatureDisabled {
                    service: service.service_name.clone(),
                    mode: service.mode.clone(),
                    feature,
                });
            }
            (Some(feature), RequestOrigin::Default) => {
                println!(
                    "{}: the mirrord operator doesn't support {feature}, running against the \
                     source DB",
                    service.service_name
                );
                service.config.feature.db_branches = DatabaseBranchesConfig::default();
                continue;
            }
            _ => {}
        }

        let namespace = service
            .config
            .target
            .namespace
            .as_deref()
            .unwrap_or(api.client().default_namespace())
            .to_owned();
        for entry in &resolved.entries {
            if let (Some(branch), Some(mode)) = (
                entry.base().and_then(|base| base.id.clone()),
                copy_modes(entry),
            ) {
                let engine = <&str>::from(DatabaseBranchEngine::from(entry));
                branch_modes
                    .entry((
                        api.cluster_url().to_owned(),
                        namespace.clone(),
                        engine,
                        branch,
                    ))
                    .or_default()
                    .push((service.service_name.clone(), mode));
            }
        }
        for entry in &response.entries {
            entry_namespaces
                .entry(entry.id.clone())
                .or_default()
                .entry((api.cluster_url().to_owned(), namespace.clone()))
                .or_default()
                .push(service.service_name.clone());
        }
    }

    // The first service to start creates the branch with its mode, and the others fail when they
    // attach to it, after some have already started. Every request is known here, so the run
    // stops before any of them.
    for ((_, _, _, branch), requests) in branch_modes {
        let mut modes = requests.iter().map(|(_, mode)| mode);
        let first = modes.next();
        if modes.any(|mode| Some(mode) != first) {
            return Err(UpCliError::BranchCopyModeConflict {
                branch,
                requests: requests
                    .iter()
                    .map(|(service, modes)| format!("`{service}` {modes}"))
                    .collect::<Vec<_>>()
                    .join(", "),
            });
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

/// The copy modes a branch is created with, compared as a session attaching to it compares them:
/// the entry's own and, for PostgreSQL, each additional database's (`schema, analytics: empty`).
/// `None` for engines without copy modes.
fn copy_modes(entry: &DatabaseBranchConfig) -> Option<String> {
    let mut modes = <&str>::from(entry.copy_mode()?).to_owned();
    for database in entry
        .pg_additional_databases()
        .iter()
        .sorted_by(|a, b| a.name.cmp(&b.name))
    {
        modes.push_str(&format!(
            ", {}: {}",
            database.name,
            <&str>::from(database.copy.mode())
        ));
    }
    Some(modes)
}

/// The settings that decide which operator a session finds and whether it has to use it, so
/// services that agree on them share one [`usable_operator`] answer.
pub(super) type OperatorKey = (Option<String>, Option<String>, Option<bool>, Option<bool>);

fn operator_key(config: &LayerConfig) -> OperatorKey {
    (
        config.kubeconfig.clone(),
        config.kube_context.clone(),
        config.accept_invalid_certificates,
        config.operator,
    )
}

/// The operator a session with `config` would use, or why there is none, decided as the session
/// decides it ([`operator_availability`]). So a service only requests branching when its session
/// will be able to, and an operator the config requires but can't be used fails here, before any
/// session starts, with the session's error.
async fn usable_operator(
    config: &LayerConfig,
) -> Result<Result<OperatorApi<NoClientCert>, NoOperator>, CliError> {
    let required = config.operator == Some(true);
    match operator_availability(
        config,
        &mut NullReporter::default(),
        &NullProgress,
        &NullProgress,
    )
    .await?
    {
        OperatorAvailability::Usable(api) => Ok(Ok(*api)),
        OperatorAvailability::NotInstalled if required => Err(CliError::OperatorNotInstalled),
        OperatorAvailability::LicenseInvalid(error) if required => Err(error.into()),
        OperatorAvailability::Disabled => Ok(Err(NoOperator::Disabled)),
        OperatorAvailability::NotInstalled => Ok(Err(NoOperator::NotInstalled)),
        OperatorAvailability::LicenseInvalid(_) => Ok(Err(NoOperator::LicenseInvalid)),
    }
}
