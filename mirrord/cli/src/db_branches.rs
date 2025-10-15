use std::collections::HashSet;

use kube::{
    Api, Client,
    api::{DeleteParams, ListParams},
};
use mirrord_operator::crd::mysql_branching::MysqlBranchDatabase;
use mirrord_progress::{Progress, ProgressTracker};
use prettytable::{Table, row};

use crate::{
    CliResult,
    config::{DbBranchesArgs, DbBranchesCommand},
};

pub async fn db_branches_command(args: DbBranchesArgs) -> CliResult<()> {
    match &args.command {
        DbBranchesCommand::Status { names } => status_command(&args, names).await,
        DbBranchesCommand::Destroy { all, names } => destroy_command(&args, *all, names).await,
    }
}

async fn status_command(args: &DbBranchesArgs, names: &Vec<String>) -> CliResult<()> {
    let names: HashSet<_> = names.into_iter().collect();

    let mut progress = ProgressTracker::from_env("DB Branches Status");
    let mut status_progress = progress.subtask("fetching branches");

    let client = Client::try_default().await.map_err(|e| {
        crate::CliError::CreateKubeApiFailed(mirrord_kube::error::KubeApiError::KubeError(e))
    })?;

    let api: Api<MysqlBranchDatabase> = if args.all_namespaces {
        Api::all(client)
    } else if let Some(namespace) = &args.namespace {
        Api::namespaced(client, namespace)
    } else {
        Api::default_namespaced(client)
    };

    let list_params = ListParams::default();

    let branches = api.list(&list_params).await.map_err(|e| {
        status_progress.failure(Some("failed to list branches"));
        crate::CliError::ListTargetsFailed(mirrord_kube::error::KubeApiError::KubeError(e))
    })?;

    // Filter by names if specified
    let filtered_branches: Vec<_> = if names.is_empty() {
        branches.into_iter().collect()
    } else {
        branches
            .into_iter()
            .filter(|branch| {
                branch
                    .metadata
                    .name
                    .as_ref()
                    .is_some_and(|name| names.contains(name))
            })
            .collect()
    };

    if filtered_branches.is_empty() {
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

    for branch in filtered_branches {
        let name = branch.metadata.name.as_deref().unwrap_or("<unknown>");
        let db_type = "MySQL";
        let phase = branch
            .status
            .as_ref()
            .map(|s| s.phase.to_string())
            .unwrap_or_else(|| "Unknown".to_string());
        let expire_time = branch
            .status
            .as_ref()
            .map(|s| s.expire_time.0.to_rfc3339())
            .unwrap_or_else(|| "Unknown".to_string());
        let ttl = branch.spec.ttl_secs.to_string();
        let db_name = branch.spec.database_name.as_deref().unwrap_or("<none>");
        let users = branch
            .status
            .as_ref()
            .map(|s| {
                if s.session_info.is_empty() {
                    "none".to_string()
                } else {
                    let mut user_list: Vec<_> = s
                        .session_info
                        .values()
                        .map(|session| session.owner.k8s_username.clone())
                        .collect();

                    user_list.sort();
                    user_list.join("\n")
                }
            })
            .unwrap_or_else(|| "none".to_string());

        table.add_row(row![name, db_type, phase, ttl, db_name, users, expire_time]);
    }

    table.printstd();

    Ok(())
}

async fn destroy_command(args: &DbBranchesArgs, all: bool, names: &Vec<String>) -> CliResult<()> {
    let mut progress = ProgressTracker::from_env("DB Branches Destroy");
    let mut destroy_progress = progress.subtask("deleting branches");

    let client = Client::try_default().await.map_err(|e| {
        crate::CliError::CreateKubeApiFailed(mirrord_kube::error::KubeApiError::KubeError(e))
    })?;

    let api: Api<MysqlBranchDatabase> = if args.all_namespaces {
        Api::all(client)
    } else if let Some(namespace) = &args.namespace {
        Api::namespaced(client, namespace)
    } else {
        Api::default_namespaced(client)
    };

    if all {
        // List all branches first to check if any exist
        let branches = api.list(&ListParams::default()).await.map_err(|e| {
            crate::CliError::ListTargetsFailed(mirrord_kube::error::KubeApiError::KubeError(e))
        })?;

        if branches.items.is_empty() {
            destroy_progress.success(Some("No active DB branch found."));
        }

        // Delete all branches
        let delete_params = DeleteParams::default();
        for branch in branches {
            if let Some(name) = branch.metadata.name {
                let mut branch_progress = destroy_progress.subtask(&format!("destroying {name}"));
                match api.delete(&name, &delete_params).await {
                    Ok(_) => branch_progress.success(Some(&format!("destroyed {name}"))),
                    Err(e) => branch_progress.failure(Some(&format!("failed: {e}"))),
                }
            }
        }
        destroy_progress.success(None);
    } else {
        let delete_params = DeleteParams::default();

        for name in names {
            let mut branch_progress = destroy_progress.subtask(&format!("destroying {name}"));
            match api.get(&name).await {
                Ok(_) => match api.delete(&name, &delete_params).await {
                    Ok(_) => branch_progress.success(Some(&format!("destroyed branch: {name}"))),
                    Err(e) => branch_progress
                        .failure(Some(&format!("failed to destroy branch {name}: {e}"))),
                },
                Err(kube::Error::Api(api_err)) if api_err.code == 404 => {
                    destroy_progress.failure(Some(&format!("branch not found: {name}")))
                }
                Err(e) => {
                    return Err(crate::CliError::ListTargetsFailed(
                        mirrord_kube::error::KubeApiError::KubeError(e),
                    ));
                }
            }
        }
        destroy_progress.success(None);
    }

    progress.success(None);

    Ok(())
}
