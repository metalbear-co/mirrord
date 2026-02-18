use std::{collections::HashSet, fmt::Debug};

use k8s_openapi::NamespaceResourceScope;
use kube::{Api, Resource, api::DeleteParams};
use mirrord_config::{LayerConfig, config::ConfigContext};
use mirrord_operator::crd::branching::{mysql::MysqlBranchDatabase, pg::PgBranchDatabase};
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
    db_type: &'static str,
    phase: Option<String>,
    ttl: u64,
    database: Option<String>,
    users: Option<String>,
    expire_time: Option<String>,
}

impl From<&MysqlBranchDatabase> for BranchInfo {
    fn from(branch: &MysqlBranchDatabase) -> Self {
        Self {
            name: branch.metadata.name.clone().unwrap_or_default(),
            db_type: "MySQL",
            phase: branch.status.as_ref().map(|s| s.phase.to_string()),
            ttl: branch.spec.ttl_secs,
            database: branch.spec.database_name.clone(),
            users: branch.status.as_ref().and_then(|s| {
                if s.session_info.is_empty() {
                    None
                } else {
                    let mut user_list: Vec<_> = s
                        .session_info
                        .values()
                        .map(|session| session.owner.k8s_username.clone())
                        .collect();
                    user_list.sort();
                    Some(user_list.join("\n"))
                }
            }),
            expire_time: branch.status.as_ref().map(|s| s.expire_time.0.to_rfc3339()),
        }
    }
}

impl From<MysqlBranchDatabase> for BranchInfo {
    fn from(
        MysqlBranchDatabase {
            metadata,
            spec,
            mut status,
        }: MysqlBranchDatabase,
    ) -> Self {
        Self {
            name: metadata.name.unwrap_or_default(),
            db_type: "MySQL",
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

impl From<&PgBranchDatabase> for BranchInfo {
    fn from(branch: &PgBranchDatabase) -> Self {
        Self {
            name: branch.metadata.name.clone().unwrap_or_default(),
            db_type: "PostgreSQL",
            phase: branch.status.as_ref().map(|s| s.phase.to_string()),
            ttl: branch.spec.ttl_secs,
            database: branch.spec.database_name.clone(),
            users: branch.status.as_ref().and_then(|s| {
                if s.session_info.is_empty() {
                    None
                } else {
                    let mut user_list: Vec<_> = s
                        .session_info
                        .values()
                        .map(|session| session.owner.k8s_username.clone())
                        .collect();
                    user_list.sort();
                    Some(user_list.join("\n"))
                }
            }),
            expire_time: branch.status.as_ref().map(|s| s.expire_time.0.to_rfc3339()),
        }
    }
}

impl From<PgBranchDatabase> for BranchInfo {
    fn from(
        PgBranchDatabase {
            metadata,
            spec,
            mut status,
        }: PgBranchDatabase,
    ) -> Self {
        Self {
            name: metadata.name.unwrap_or_default(),
            db_type: "PostgreSQL",
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

    let mysql_api: Api<MysqlBranchDatabase> = get_api(args, &client, &layer_config);

    let pg_api: Api<PgBranchDatabase> = get_api(args, &client, &layer_config);

    let mysql_branches = list_resource_if_defined(&mysql_api, &mut status_progress)
        .await?
        .unwrap_or_default();

    let pg_branches = list_resource_if_defined(&pg_api, &mut status_progress)
        .await?
        .unwrap_or_default();

    if mysql_branches.is_empty() && pg_branches.is_empty() {
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

    // When names are provided, only show matching branches.
    // When no names are given, show all branches (empty set means no filter).
    fn get_iter<T: Resource + Into<BranchInfo>>(
        vec: Vec<T>,
        names: &HashSet<&String>,
    ) -> impl Iterator<Item = BranchInfo> {
        vec.into_iter()
            .filter(|branch| {
                if names.is_empty() {
                    return true;
                }

                branch
                    .meta()
                    .name
                    .as_ref()
                    .map(|name| names.contains(name))
                    .unwrap_or_default()
            })
            .map(Into::into)
    }

    let mysql_iter = get_iter(mysql_branches, &names);
    let pg_iter = get_iter(pg_branches, &names);
    let branch_iter = mysql_iter.chain(pg_iter);

    for branch in branch_iter {
        table.add_row(row![
            branch.name,
            branch.db_type,
            branch.phase.unwrap_or_else(|| "Unknown".to_string()),
            branch.ttl,
            branch.database.unwrap_or_else(|| "<none>".to_string()),
            branch.users.unwrap_or_else(|| "none".to_string()),
            branch.expire_time.unwrap_or_else(|| "Unknown".to_string())
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

    let mysql_api: Api<MysqlBranchDatabase> = get_api(args, &client, &layer_config);

    let pg_api: Api<PgBranchDatabase> = get_api(args, &client, &layer_config);

    if all {
        // List all branches first to check if any exist

        let mysql_branches = list_resource_if_defined(&mysql_api, &mut destroy_progress).await?;

        let pg_branches = list_resource_if_defined(&pg_api, &mut destroy_progress).await?;

        if mysql_branches.as_ref().is_none_or(|vec| vec.is_empty())
            && pg_branches.as_ref().is_none_or(|vec| vec.is_empty())
        {
            destroy_progress.success(Some("No active DB branch found."));
        } else {
            // Delete all MySQL branches
            let d_params = DeleteParams::default();
            let my_branch_names = mysql_branches
                .into_iter()
                .flatten()
                .filter_map(|b| b.metadata.name);
            delete_branches(my_branch_names, &mysql_api, &destroy_progress, &d_params).await;

            // Delete all Postgres branches
            let pg_branch_names = pg_branches
                .into_iter()
                .flatten()
                .filter_map(|b| b.metadata.name);
            delete_branches(pg_branch_names, &pg_api, &destroy_progress, &d_params).await;

            destroy_progress.success(None);
        }
    } else {
        // First, list all branches to determine their types
        let mysql_branches = list_resource_if_defined(&mysql_api, &mut destroy_progress).await?;

        let pg_branches = list_resource_if_defined(&pg_api, &mut destroy_progress).await?;

        let mut names: HashSet<_> = names.iter().collect();
        let d_params = DeleteParams::default();

        let mysql_names = mysql_branches
            .into_iter()
            .flatten()
            .filter_map(|b| b.metadata.name)
            .filter(|name| names.remove(name));
        delete_branches(mysql_names, &mysql_api, &destroy_progress, &d_params).await;

        let pg_names = pg_branches
            .into_iter()
            .flatten()
            .filter_map(|b| b.metadata.name)
            .filter(|name| names.remove(name));
        delete_branches(pg_names, &pg_api, &destroy_progress, &d_params).await;

        for name in names {
            destroy_progress.failure(Some(&format!("branch not found: {name}")));
        }

        destroy_progress.success(None);
    }

    progress.success(None);

    Ok(())
}
