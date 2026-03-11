use std::{collections::HashSet, fmt::Debug};

use k8s_openapi::NamespaceResourceScope;
use kube::{Api, Resource, api::DeleteParams};
use mirrord_config::{LayerConfig, config::ConfigContext};
use mirrord_operator::crd::db_branching::branch_database::BranchDatabase;
use mirrord_progress::{Progress, ProgressTracker};
use prettytable::{Table, row};
use serde::de::DeserializeOwned;

use crate::{
    CliResult,
    config::{DbBranchesArgs, DbBranchesCommand},
    kube::{kube_client_from_layer_config, list_resource_if_defined},
};

#[derive(Debug)]
struct BranchInfo {
    name: String,
    db_type: String,
    phase: Option<String>,
    ttl: u64,
    database: Option<String>,
    users: Option<String>,
    expire_time: Option<String>,
}

impl From<BranchDatabase> for BranchInfo {
    fn from(
        BranchDatabase {
            metadata,
            spec,
            mut status,
        }: BranchDatabase,
    ) -> Self {
        Self {
            name: metadata.name.unwrap_or_default(),
            db_type: spec
                .dialect()
                .map(|d| d.to_string())
                .unwrap_or_else(|_| "Unknown".to_string()),
            phase: status.as_ref().map(|s| s.phase.to_string()),
            ttl: spec.ttl_secs,
            database: spec.database_name,
            users: status.as_mut().and_then(|s| {
                if s.session_info.is_empty() {
                    None
                } else {
                    let mut user_list: Vec<_> = std::mem::take(&mut s.session_info)
                        .into_values()
                        .map(|session| session.owner.k8s_username)
                        .collect();
                    user_list.sort();
                    Some(user_list.join("\n"))
                }
            }),
            expire_time: status.as_ref().map(|s| s.expire_time.0.to_rfc3339()),
        }
    }
}

pub async fn db_branches_command(args: DbBranchesArgs) -> CliResult<()> {
    match &args.command {
        DbBranchesCommand::Status { names } => status_command(&args, names.as_slice()).await,
        DbBranchesCommand::Destroy { all, names } => destroy_command(&args, *all, names).await,
    }
}

fn get_api<T: Resource<DynamicType = (), Scope = NamespaceResourceScope>>(
    args: &DbBranchesArgs,
    client: &kube::Client,
    layer_config: &LayerConfig,
) -> Api<T> {
    if args.all_namespaces {
        Api::all(client.clone())
    } else if let Some(namespace) = &args.namespace {
        Api::namespaced(client.clone(), namespace)
    } else if let Some(namespace) = &layer_config.target.namespace {
        Api::namespaced(client.clone(), namespace)
    } else {
        Api::default_namespaced(client.clone())
    }
}

async fn status_command(args: &DbBranchesArgs, names: &[String]) -> CliResult<()> {
    let names: HashSet<_> = names.iter().collect();

    let mut progress = ProgressTracker::from_env("DB Branches Status");
    let mut status_progress = progress.subtask("fetching branches");

    let mut cfg_context = ConfigContext::default()
        .override_env_opt(LayerConfig::FILE_PATH_ENV, args.config_file.clone())
        .override_env_opt("MIRRORD_TARGET_NAMESPACE", args.namespace.clone());

    let layer_config = LayerConfig::resolve(&mut cfg_context)?;

    let client = kube_client_from_layer_config(&layer_config).await?;

    let branch_api: Api<BranchDatabase> = get_api(args, &client, &layer_config);

    let branches = list_resource_if_defined(&branch_api, &mut status_progress)
        .await?
        .unwrap_or_default();

    if branches.is_empty() {
        progress.success(Some("No active DB branch found"));
        return Ok(());
    }

    status_progress.success(Some("fetched status"));
    progress.success(None);

    let mut table = Table::new();
    table.add_row(row![
        "Name",
        "DB Type",
        "Phase",
        "TTL (sec)",
        "Database",
        "Users",
        "Expires At"
    ]);

    let branch_iter = branches.into_iter().filter(|branch| {
        if names.is_empty() {
            return true;
        }
        branch
            .meta()
            .name
            .as_ref()
            .map(|name| names.contains(name))
            .unwrap_or_default()
    });

    for branch in branch_iter {
        let info = BranchInfo::from(branch);
        table.add_row(row![
            info.name,
            info.db_type,
            info.phase.unwrap_or_else(|| "Unknown".to_string()),
            info.ttl,
            info.database.unwrap_or_else(|| "<none>".to_string()),
            info.users.unwrap_or_else(|| "none".to_string()),
            info.expire_time.unwrap_or_else(|| "Unknown".to_string())
        ]);
    }

    table.printstd();

    Ok(())
}

async fn delete_branches<I, R, P>(
    branch_names: I,
    api: &kube::Api<R>,
    progress: &P,
    delete_params: &DeleteParams,
) where
    I: Iterator<Item = String>,
    R: Resource<DynamicType = ()> + Clone + DeserializeOwned + Debug,
    P: Progress,
{
    for branch in branch_names {
        let mut branch_progress =
            progress.subtask(&format!("destroying {} {branch}", R::kind(&())));
        match api.delete(&branch, delete_params).await {
            Ok(_) => branch_progress.success(Some(&format!("destroyed {branch}"))),
            Err(e) => branch_progress.failure(Some(&format!("failed: {e}"))),
        }
    }
}

async fn destroy_command(args: &DbBranchesArgs, all: bool, names: &[String]) -> CliResult<()> {
    let mut progress = ProgressTracker::from_env("DB Branches Destroy");
    let mut destroy_progress = progress.subtask("deleting branches");

    let mut cfg_context = ConfigContext::default()
        .override_env_opt(LayerConfig::FILE_PATH_ENV, args.config_file.clone())
        .override_env_opt("MIRRORD_TARGET_NAMESPACE", args.namespace.clone());

    let layer_config = LayerConfig::resolve(&mut cfg_context)?;

    let client = kube_client_from_layer_config(&layer_config).await?;

    let branch_api: Api<BranchDatabase> = get_api(args, &client, &layer_config);

    if all {
        let branches = list_resource_if_defined(&branch_api, &mut destroy_progress).await?;

        if branches.as_ref().is_none_or(|vec| vec.is_empty()) {
            destroy_progress.success(Some("No active DB branch found."));
        } else {
            let d_params = DeleteParams::default();
            let branch_names = branches
                .into_iter()
                .flatten()
                .filter_map(|b| b.metadata.name);
            delete_branches(branch_names, &branch_api, &destroy_progress, &d_params).await;

            destroy_progress.success(None);
        }
    } else {
        let branches = list_resource_if_defined(&branch_api, &mut destroy_progress).await?;

        let mut names: HashSet<_> = names.iter().collect();
        let d_params = DeleteParams::default();

        let matching_names = branches
            .into_iter()
            .flatten()
            .filter_map(|b| b.metadata.name)
            .filter(|name| names.remove(name));
        delete_branches(matching_names, &branch_api, &destroy_progress, &d_params).await;

        for name in names {
            destroy_progress.failure(Some(&format!("branch not found: {name}")));
        }

        destroy_progress.success(None);
    }

    progress.success(None);

    Ok(())
}
