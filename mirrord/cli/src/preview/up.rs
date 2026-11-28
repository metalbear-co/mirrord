//! `mirrord preview up` and `mirrord preview sync`: start a preview environment for every
//! service of a `mirrord-up.yaml` that the command picks (see [`PreviewRun`]), all under one key,
//! and return once each one is ready, idle or failed. `mirrord preview sync` also pushes each
//! service's `sync` directories into its preview (see [`super::sync`]), and with `--watch` keeps
//! pushing them. `mirrord preview down` deletes the previews again, and `mirrord preview wake`
//! (see [`super::wake`]) finds them the same way.
//!
//! Every service's session is created the way `mirrord preview start` creates one, from the
//! config `mirrord_up::resolve_preview_services` assembles for it. All sessions are prepared
//! first, and none is started unless every one passed its checks, so an invalid service never
//! leaves the others replaced. A session that fails to start doesn't stop the others.

use std::{io::ErrorKind, ops::Not, path::Path, sync::Arc};

use futures::future::{BoxFuture, join_all, try_join_all};
use itertools::Itertools;
use kube::{Api, Client, api::ListParams};
use miette::Diagnostic;
use mirrord_analytics::{AnalyticsReporter, ExecutionKind, ReportTarget, Reporter};
use mirrord_config::{
    config::{ConfigContext, EnvKey},
    env_key::MIRRORD_ENV_KEY,
    feature::{
        database_branches::{DatabaseBranchesConfig, SplitConfigDbBranches, SplitConfigRequest},
        split_queues::SplitQueuesConfig,
    },
    target::{Target, TargetType},
    util::GIT_BRANCH,
};
use mirrord_kube::error::KubeApiError;
use mirrord_operator::{
    client::{
        OperatorAccessError, active_operator_login, error::OperatorApiError,
        required_branching_feature, split_config_branches::entries_from_response,
    },
    crd::{NewOperatorFeature, TargetCrd},
    sync::UnpairReason,
};
use mirrord_progress::{Progress, ProgressTracker};
use mirrord_up::{
    AutoscaleOrigin, INFERRED_TARGET_TYPES, PreviewImageSource, PreviewRun, RequestOrigin,
    SkipReason, SubprocessCfg, UpError, WorkloadLister, load_preview_up_config, preview_image_env,
};

use super::{
    PreparedPreview, StopMode, create_preview_api, load_manifests, manifest_sources,
    preview_namespace, preview_stop, report_local_feature_gate, sync, sync::ServiceSync,
    sync_task_error,
};
use crate::{
    config::{
        DEFAULT_PREVIEW_CONFIG_FILE, ManifestSourceArgs, PreviewCommonArgs, PreviewStopArgs,
        PreviewUpArgs,
    },
    data::{GlobalConfig, UserData},
    error::{CliError, CliResult, GENERAL_HELP},
    up::checks::{DEFAULT_DB_BRANCHES_UNSUPPORTED, resolves_default_db_branches},
};

/// Handle `mirrord preview up` or `mirrord preview sync`, which `run` names, reporting one
/// `PreviewUp` event for the run. With `watch_files`, the `--watch` of `mirrord preview sync`,
/// the run keeps pushing the `sync` directories after the previews are up.
pub(super) async fn preview_up(
    common: &PreviewCommonArgs,
    args: PreviewUpArgs,
    run: PreviewRun,
    watch_files: bool,
    watch: drain::Watch,
    user_data: &UserData,
) -> CliResult<()> {
    // The `mirrord-up.yaml` that can turn telemetry off isn't read yet. Enabled until it is, so a
    // run that fails to read it is still reported.
    let mut analytics = AnalyticsReporter::for_event(
        ReportTarget::PreviewUp,
        true,
        watch.clone(),
        user_data.machine_id(),
    );

    analytics.get_mut().add("sync", run == PreviewRun::Sync);

    let result = run_preview_up(
        common,
        args,
        run,
        watch_files,
        watch,
        user_data,
        &mut analytics,
    )
    .await;

    analytics.get_mut().add("success", result.is_ok());
    if let Err(error) = &result {
        analytics
            .get_mut()
            .add("error_category", ErrorCategory::from(error) as u32);
    }

    result
}

/// Coarse bucket of a failed `mirrord preview up` or `mirrord preview sync` run, for telemetry.
#[derive(Clone, Copy, Debug)]
#[repr(u32)]
enum ErrorCategory {
    /// Before any session was prepared: reading the `mirrord-up.yaml`, picking the images or
    /// resolving the targets.
    Setup = 1,
    /// A service failed its checks, so no session was started.
    Checks = 2,
    /// A session failed to start.
    Start = 3,
}

impl From<&CliError> for ErrorCategory {
    fn from(error: &CliError) -> Self {
        match error {
            CliError::PreviewUpInvalid(_) => Self::Checks,
            CliError::PreviewUpFailed(_) | CliError::PreviewSyncStartFailed(_) => Self::Start,
            _ => Self::Setup,
        }
    }
}

async fn run_preview_up(
    common: &PreviewCommonArgs,
    args: PreviewUpArgs,
    run: PreviewRun,
    watch_files: bool,
    watch: drain::Watch,
    user_data: &UserData,
    analytics: &mut AnalyticsReporter,
) -> CliResult<()> {
    let mut progress = ProgressTracker::from_env(run.into());

    let key = match findable_key_from_env(common.key.as_deref()) {
        Some(key) => EnvKey::Provided(key),
        None => EnvKey::Generated(EnvKey::autogenerated()),
    };
    progress.info(&format!("key: {}", key.as_str()));

    let path = up_file_path(common.config_file.as_deref());
    let mut up_config = match load_preview_up_config(path, &key) {
        Err(UpError::Io(error)) if error.kind() == ErrorKind::NotFound => {
            return Err(CliError::PreviewUpFileNotFound(path.to_owned()));
        }
        result => result?,
    };
    up_config.default_context(GlobalConfig::load().await.kube_context().map(Arc::from));
    analytics.enabled = up_config.telemetry_enabled();

    let mut images =
        up_config.select_preview_images(&args.image, &ConfigContext::default(), run)?;
    let analytics_data = analytics.get_mut();
    analytics_data.add("services_previewed", up_config.services.len());
    analytics_data.add("services_skipped", images.skipped.len());
    for (property, source) in [
        ("images_from_flag", PreviewImageSource::Flag),
        ("images_from_env", PreviewImageSource::Env),
    ] {
        let count = images
            .picked
            .values()
            .filter(|(_, picked)| *picked == source)
            .count();
        analytics_data.add(property, count);
    }
    for (service, reason) in &images.skipped {
        let reason = match reason {
            SkipReason::NoImage => format!(
                "no image (`--image {service}=<image ref>` or `{}`)",
                preview_image_env(service)
            ),
            SkipReason::Skip => "`skip: true`".to_owned(),
            SkipReason::NoSync => "no `sync`".to_owned(),
        };
        progress.info(&format!("{service}: {reason}, skipped"));
    }

    let lister = operator_workloads(up_config.common.accept_invalid_certificates()).await?;
    let mut services = mirrord_up::resolve_preview_services(
        up_config,
        path,
        common.context.as_deref().map(Arc::from),
        &key,
        lister.as_ref().map(|lister| lister as &dyn WorkloadLister),
    )
    .await?;
    for service in &mut services {
        service.config.feature.preview.image = images
            .picked
            .remove(&service.service_name)
            .map(|(image, _)| image);
    }
    services.sort_by(|a, b| a.service_name.cmp(&b.service_name));

    let args = &args;
    let prepared = join_all(services.into_iter().map(|service| {
        let name = service.service_name.clone();
        let progress = progress.subtask(&name).prefixed(&name);
        let watch = watch.clone();
        async move {
            let result = prepare_service(service, args, run, &progress, watch, user_data).await;
            (name, progress, result)
        }
    }))
    .await;

    let mut failed = Vec::new();
    let mut checked = Vec::new();
    for (name, mut service_progress, result) in prepared {
        match result {
            Ok(preview) => checked.push(ServicePreview {
                name,
                progress: service_progress,
                preview,
            }),
            Err(error) => {
                // The run fails with `PreviewUpInvalid`, which only names the services, so each
                // service's line also says how to fix it, without the general help that every
                // `CliError` appends, which would repeat on every line.
                let help = Diagnostic::help(&error).map(|help| help.to_string());
                let message = match help
                    .as_deref()
                    .map(|help| help.trim_end_matches(GENERAL_HELP))
                {
                    Some(help) if help.is_empty().not() => format!("{error}\n{help}"),
                    _ => error.to_string(),
                };
                service_progress.failure(Some(&message));
                failed.push(name.to_string());
            }
        }
    }

    // Compared the way `start` finds the session it replaces: sessions are listed in the
    // namespace they're stored in and matched on their target. The cluster stands in for the
    // kube context, which can have several names.
    let same_target = |a: &PreparedPreview, b: &PreparedPreview| {
        a.operator_api.cluster_url() == b.operator_api.cluster_url()
            && preview_namespace(&a.operator_api, &a.layer_config)
                == preview_namespace(&b.operator_api, &b.layer_config)
            && a.spec.target == b.spec.target
    };
    // Service names are the keys of `services`, so they tell the services apart.
    let clashes = checked
        .iter()
        .map(|service| {
            checked
                .iter()
                .find(|other| {
                    other.name != service.name && same_target(&service.preview, &other.preview)
                })
                .map(|other| other.name.clone())
        })
        .collect::<Vec<_>>();

    let mut ready_to_start = Vec::new();
    for (mut service, clash) in checked.into_iter().zip(clashes) {
        match clash {
            Some(other) => {
                service.progress.failure(Some(&format!(
                    "has the same target as `{other}`: sessions under one key and target \
                     replace each other"
                )));
                failed.push(service.name.to_string());
            }
            None => ready_to_start.push(service),
        }
    }
    failed.sort();

    if failed.is_empty().not() {
        analytics.get_mut().add("services_failed", failed.len());
        for mut service in ready_to_start {
            service.progress.failure(Some("not started"));
        }
        progress.failure(Some("no preview environment was started"));
        return Err(CliError::PreviewUpInvalid(failed));
    }

    let started = join_all(ready_to_start.into_iter().map(|mut service| async move {
        let result = service
            .preview
            .start(watch_files, &mut service.progress)
            .await;
        (service.name, service.progress, result)
    }))
    .await;

    let mut syncs = Vec::new();
    for (name, mut service_progress, result) in started {
        match result {
            Ok((summary, sync_task)) => {
                syncs.extend(sync_task.map(|sync_task| (name.clone(), sync_task)));
                summary.report(&service_progress);
                summary.report_scaling(&service_progress, "`preview.autoscale` in mirrord-up.yaml");
                service_progress.success(Some(if summary.idle {
                    "preview environment is idle"
                } else {
                    "preview environment is ready"
                }));
            }
            Err(error) => {
                service_progress.failure(Some(&sync::with_causes(&error)));
                failed.push(name.to_string());
            }
        }
    }

    analytics.get_mut().add("services_failed", failed.len());
    if failed.is_empty().not() {
        progress.failure(Some("some preview environments failed to start"));
        return Err(match run {
            PreviewRun::Up => CliError::PreviewUpFailed(failed),
            PreviewRun::Sync => CliError::PreviewSyncStartFailed(failed),
        });
    }

    if watch_files.not() || syncs.is_empty() {
        progress.success(Some("preview environments are up"));
        return Ok(());
    }

    progress.info("watching the `sync` directories for changes, press Ctrl-C to stop");
    // A sync only ends when it fails, when another CLI takes it over, or when its session is
    // deleted, and the last two stop only that service's sync. The first failure stops the others
    // and fails the run: dropping a sync's task stops it.
    let watches = try_join_all(syncs.into_iter().map(|(name, sync_task)| {
        let mut sync_progress = progress.subtask("watching for changes").prefixed(&name);
        async move {
            match sync_task.await {
                Ok(CliError::PreviewSyncReplaced) => {
                    sync_progress.success(Some(&format!(
                        "{}, so this one stopped and left the preview environment as it is",
                        UnpairReason::Replaced
                    )));
                    Ok(())
                }
                Ok(CliError::PreviewSessionDeleted) => {
                    sync_progress.success(Some(
                        "the preview session was deleted, so `mirrord preview sync` stopped \
                         syncing into it",
                    ));
                    Ok(())
                }
                result => {
                    let error = sync_task_error(result);
                    sync_progress.failure(Some(&sync::with_causes(&error)));
                    Err(error)
                }
            }
        }
    }));

    tokio::select! {
        result = watches => match result {
            Ok(_) => {
                progress.success(Some("stopped watching"));
                Ok(())
            }
            Err(error) => {
                progress.failure(Some("stopped syncing changes"));
                Err(error)
            }
        },
        _ = tokio::signal::ctrl_c() => {
            progress.success(Some("stopped watching"));
            Ok(())
        }
    }
}

/// A service whose preview passed its checks, with the task its progress is reported on.
struct ServicePreview {
    name: Arc<str>,
    progress: ProgressTracker,
    preview: PreparedPreview,
}

/// Applies the run's flags to one service's config and runs every check that doesn't change the
/// cluster, dropping the defaults `mirrord up` added that its operator can't serve.
async fn prepare_service(
    service: SubprocessCfg,
    args: &PreviewUpArgs,
    run: PreviewRun,
    progress: &ProgressTracker,
    watch: drain::Watch,
    user_data: &UserData,
) -> CliResult<PreparedPreview> {
    // `mirrord preview up` ignores `sync`, which only `mirrord preview sync` pushes.
    let sync = match run {
        PreviewRun::Up => None,
        PreviewRun::Sync => ServiceSync::of_service(&service)?,
    };
    let SubprocessCfg {
        mut config,
        service_name,
        db_branches_origin,
        queues_origin,
        autoscale_origin,
        ..
    } = service;
    let default_db_branches = db_branches_origin == RequestOrigin::Default;
    let default_split_queues = queues_origin == RequestOrigin::Default;

    // Set over the patched config, so they take precedence over the service's `config_patch`.
    if let Some(ttl) = args.ttl {
        config.feature.preview.ttl_mins = Some(ttl);
        config.feature.preview.ttl_secs = None;
    }
    if let Some(timeout) = args.timeout {
        config.feature.preview.creation_timeout_secs = timeout;
    }

    // Only one pod at a time can write the code volume, so a sync service runs one pod even when
    // `common.preview.autoscale` asks every service to scale; the service's own setting was
    // already refused by `ServiceSync::of_service`.
    if sync.is_some() && autoscale_origin == Some(AutoscaleOrigin::Common) {
        config.feature.preview.autoscale = None;
        progress.info("`common.preview.autoscale` doesn't apply to a service with `sync`: one pod writes the code volume");
    }

    if let Some(target) = &config.target.path {
        progress.info(&format!("target: {target}"));
    }

    // `mirrord up` assembles the config without it, so the preview checks below would take
    // the presets for options the user set.
    config.apply_magic();

    let mut context = ConfigContext::default().strict_env(true);
    let verified = config.verify_for_preview_env(&mut context);
    for warning in context.into_warnings() {
        progress.warning(&warning);
    }
    verified?;

    let sources = manifest_sources(ManifestSourceArgs::default(), &mut config);
    let supplied_objects = if sources.paths.is_empty() {
        None
    } else {
        Some(load_manifests(&sources.paths, progress)?)
    };

    let mut analytics = AnalyticsReporter::only_error(
        config.telemetry,
        ExecutionKind::Preview,
        watch,
        user_data.machine_id(),
        Some(config.key.as_str().to_owned()),
    );
    let (operator_api, api) =
        create_preview_api(&config, false, &[], progress, &mut analytics).await?;

    // A request the user set fails in `prepare` instead, as it does for `mirrord preview start`.
    if default_db_branches && resolves_default_db_branches(&operator_api).not() {
        progress.warning(DEFAULT_DB_BRANCHES_UNSUPPORTED);
        config.feature.db_branches = DatabaseBranchesConfig::default();
    }

    if let Some(request) = config.feature.db_branches.split_config_request().cloned() {
        let response = operator_api
            .lookup_split_config_db_branches(&config, &request)
            .await?;
        let resolved = entries_from_response(&response, config.key.as_str())?;
        let supported = operator_api.operator().spec.supported_features();
        let (branchable, off): (Vec<_>, Vec<_>) = response
            .entries
            .iter()
            .zip(resolved.entries.iter().map(required_branching_feature))
            .partition(|(_, feature)| feature.is_none_or(|feature| supported.contains(&feature)));

        if off.is_empty().not() {
            if default_db_branches.not() {
                for feature in off.iter().filter_map(|(_, feature)| *feature) {
                    if let Err(error) = operator_api.operator().spec.require_feature(feature) {
                        if let OperatorApiError::LocalFeatureNotLicensed { feature, .. } = &error {
                            report_local_feature_gate(
                                operator_api,
                                *feature,
                                &config,
                                progress,
                                &mut analytics,
                            )
                            .await;
                        }
                        return Err(error.into());
                    }
                }
            }
            progress.warning(&format!(
                "the mirrord operator has these off, so their databases are not branched: {}",
                off.iter()
                    .filter_map(|(entry, feature)| Some(format!(
                        "`{}` ({})",
                        entry.id,
                        (*feature)?
                    )))
                    .join(", ")
            ));
            config.feature.db_branches = if branchable.is_empty() {
                DatabaseBranchesConfig::default()
            } else {
                DatabaseBranchesConfig::FromSplitConfig(SplitConfigRequest::Entries(
                    SplitConfigDbBranches::Ids(
                        branchable
                            .iter()
                            .map(|(entry, _)| entry.id.clone())
                            .collect(),
                    ),
                ))
            };
        }
    }

    // An operator that advertises it drops the default splits it can't serve, as it does for
    // `mirrord up` sessions. An older one fails the session over them instead, so it gets none.
    let mut auto_queue_splitting = false;
    if default_split_queues {
        let supported = operator_api.operator().spec.supported_features();
        if supported
            .contains(&NewOperatorFeature::PreviewAutoQueueSplitting)
            .not()
        {
            progress.warning(
                "queues are not split: the mirrord operator can't split them by default for \
                 preview environments",
            );
            config.feature.split_queues = SplitQueuesConfig::default();
        } else {
            // An operator that advertises `PreviewAutoQueueSplitting` is new enough to advertise
            // every broker it has on, Azure Service Bus included, so a missing broker feature
            // means that broker is off.
            let kinds = config.feature.split_queues.kinds();
            let disabled = kinds
                .iter()
                .filter_map(|kind| {
                    let feature = NewOperatorFeature::queue_splitting(*kind)?;
                    supported.contains(&feature).not().then_some(feature)
                })
                .collect::<Vec<_>>();

            if disabled.len() == kinds.len() {
                // Sent anyway, the session would split nothing and only wait on a split session
                // that never comes.
                progress
                    .warning("queues are not split: the mirrord operator has queue splitting off");
                config.feature.split_queues = SplitQueuesConfig::default();
            } else {
                if disabled.is_empty().not() {
                    progress.warning(&format!(
                        "the mirrord operator has these off, so their queues are not split: {}",
                        disabled.iter().join(", ")
                    ));
                }
                auto_queue_splitting = true;
            }
        }
    }

    let mut prepared = PreparedPreview::prepare(
        config,
        operator_api,
        api,
        &sources,
        supplied_objects,
        auto_queue_splitting,
        sync,
        Some(&service_name),
        progress,
        analytics,
    )
    .await?;

    // `common.preview.autoscale` speaks for every service, autoscaled targets or not, so a target
    // without an autoscaler runs a fixed number of pods instead of failing the service.
    if let Some(autoscale) = prepared.spec.autoscale.as_mut() {
        autoscale.target_autoscaler_optional =
            Some(autoscale_origin == Some(AutoscaleOrigin::Common));
    }

    Ok(prepared)
}

/// The `mirrord-up.yaml` that `-f` names, `mirrord-up.yaml` by default.
///
/// `-f` is shared with the other `preview` subcommands, where a bare `-f` stands for a mirrord
/// config, which is never a valid `mirrord-up.yaml`.
fn up_file_path(config_file: Option<&Path>) -> &Path {
    match config_file {
        Some(path) if path != Path::new(DEFAULT_PREVIEW_CONFIG_FILE) => path,
        _ => Path::new("mirrord-up.yaml"),
    }
}

/// Lists the targets a service's target is inferred from through the operator, for a CLI logged
/// in to it through SSO, whose URL serves only the operator's API.
struct OperatorWorkloads {
    client: Client,
}

/// The lister for the active operator login, or `None` without one, when the services' kube
/// contexts are read instead.
async fn operator_workloads(
    accept_invalid_certificates: Option<bool>,
) -> CliResult<Option<OperatorWorkloads>> {
    let Some(login) = active_operator_login().await? else {
        return Ok(None);
    };
    let mut config = login
        .kube_config()
        .map_err(|error| OperatorAccessError::InvalidUrl {
            url: login.url.clone(),
            error,
        })?;
    if let Some(accept_invalid_certificates) = accept_invalid_certificates {
        config.accept_invalid_certs = accept_invalid_certificates;
    }
    let client = Client::try_from(config)
        .map_err(|error| CliError::CreateKubeApiFailed(KubeApiError::from(error)))?;
    Ok(Some(OperatorWorkloads { client }))
}

impl WorkloadLister for OperatorWorkloads {
    fn default_namespace(&self) -> &str {
        self.client.default_namespace()
    }

    fn list<'a>(&'a self, namespace: &'a str) -> BoxFuture<'a, Result<Vec<String>, UpError>> {
        Box::pin(async move {
            let listed = Api::<TargetCrd>::namespaced(self.client.clone(), namespace)
                .list(&ListParams::default())
                .await
                .map_err(|error| UpError::OperatorWorkloadListing {
                    namespace: namespace.to_owned(),
                    reason: error.to_string(),
                })?;
            Ok(inferable_paths(listed.items.into_iter().filter_map(
                |crd| crd.spec.target.as_known().ok().cloned(),
            )))
        })
    }
}

/// The paths of the `targets` a service's target can be inferred from, ordered like
/// [`INFERRED_TARGET_TYPES`], the order in which the kubeconfig listing finds them, so a name that
/// two kinds share resolves to the same workload either way.
fn inferable_paths(targets: impl IntoIterator<Item = Target>) -> Vec<String> {
    let mut ranked = targets
        .into_iter()
        .filter_map(|target| {
            let rank = INFERRED_TARGET_TYPES
                .iter()
                .position(|kind| *kind == TargetType::from(&target))?;
            Some((rank, target.to_string()))
        })
        .collect::<Vec<_>>();
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.into_iter().map(|(_, path)| path).collect()
}

/// Handle `mirrord preview down`: `mirrord preview stop -k <key> -A` in each scope that
/// [`key_scopes`] finds.
pub(super) async fn preview_down(
    common: &PreviewCommonArgs,
    watch: drain::Watch,
    user_data: &UserData,
) -> CliResult<()> {
    let mut result = Ok(());
    for common in key_scopes(common).await? {
        let args = PreviewStopArgs {
            glob: None,
            target: None,
            namespace: None,
            all_namespaces: true,
        };
        let stopped = preview_stop(&common, args, StopMode::Down, watch.clone(), user_data).await;
        if result.is_ok() {
            result = stopped;
        }
    }
    result
}

/// Where `mirrord preview down` and `mirrord preview wake` find the previews of a key: the key
/// found the way `mirrord preview up` finds it, in every kube context the services of the
/// `mirrord-up.yaml` run in, one [`PreviewCommonArgs`] per context, each with the key.
///
/// `mirrord preview up` and `mirrord preview sync` take each service's context from the file, so
/// this reads it too (`-f`, or `mirrord-up.yaml` when it exists) to reach them all. Without a
/// file, the only context is the one of `--context`, the global config or the kubeconfig. Every
/// preview under the key counts in those contexts, whichever services it was started for, and
/// the callers read no mirrord config, neither from `-f` nor from `MIRRORD_*` variables in the
/// environment.
pub(super) async fn key_scopes(common: &PreviewCommonArgs) -> CliResult<Vec<PreviewCommonArgs>> {
    let key =
        findable_key_from_env(common.key.as_deref()).ok_or(CliError::PreviewFindableKeyRequired)?;
    let context_arg = common.context.as_deref().map(Arc::from);

    let path = up_file_path(common.config_file.as_deref());
    let mut contexts = match load_preview_up_config(path, &EnvKey::Provided(key.clone())) {
        Ok(mut up_config) => {
            up_config.default_context(GlobalConfig::load().await.kube_context().map(Arc::from));
            up_config.service_contexts(context_arg.clone())
        }
        Err(UpError::Io(error))
            if error.kind() == ErrorKind::NotFound && common.config_file.is_none() =>
        {
            Default::default()
        }
        Err(UpError::Io(error)) if error.kind() == ErrorKind::NotFound => {
            return Err(CliError::PreviewUpFileNotFound(path.to_owned()));
        }
        Err(error) => return Err(error.into()),
    };
    // While logged in to an operator, every context reaches that one operator, so one scope
    // covers the key and no preview is listed or woken twice.
    if active_operator_login().await?.is_some() {
        contexts.clear();
    }
    if contexts.is_empty() {
        contexts.insert(context_arg);
    }

    Ok(contexts
        .into_iter()
        .map(|context| PreviewCommonArgs {
            key: Some(key.clone()),
            config_file: None,
            context: context.as_deref().map(ToOwned::to_owned),
        })
        .collect())
}

/// The key `mirrord preview up` runs under when it can be found again: `arg` (`--key`), then
/// `env_key` (`MIRRORD_KEY`), then the branch (`github_head_ref`, `GITHUB_HEAD_REF` in a GitHub
/// pull request, otherwise `git_branch`, the current git branch). `None` on `main`, `master` or
/// without a branch. Empty values count as unset.
///
/// The callers read the values, so the same input always gives the same key.
fn findable_key(
    arg: Option<&str>,
    env_key: Option<String>,
    github_head_ref: Option<String>,
    git_branch: Option<String>,
) -> Option<String> {
    let set = |value: &String| value.is_empty().not();

    arg.map(ToOwned::to_owned)
        .or_else(|| env_key.filter(set))
        .or_else(|| {
            github_head_ref
                .filter(set)
                .or(git_branch)
                .filter(|branch| matches!(branch.as_str(), "main" | "master").not())
        })
}

/// [`findable_key`] with its values read from the environment.
fn findable_key_from_env(arg: Option<&str>) -> Option<String> {
    findable_key(
        arg,
        std::env::var(MIRRORD_ENV_KEY).ok(),
        std::env::var("GITHUB_HEAD_REF").ok(),
        GIT_BRANCH.clone(),
    )
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    /// Through the operator, the targets come in the operator's order, so they are put in the
    /// kubeconfig listing's kind order: a service named like both a Deployment and a Pod still
    /// resolves to the Deployment. Kinds a service is never inferred from are left out.
    #[test]
    fn operator_targets_are_ranked_like_the_kubeconfig_listing() {
        let targets = [
            "pod/app",
            "job/app",
            "rollout/app",
            "deployment/app",
            "statefulset/db",
        ]
        .map(|path| Target::from_str(path).expect("a test target parses"));

        assert_eq!(
            inferable_paths(targets),
            vec![
                "deployment/app".to_owned(),
                "statefulset/db".to_owned(),
                "rollout/app".to_owned(),
                "pod/app".to_owned(),
            ]
        );
    }
}
