use kube::{
    Api, Client,
    api::{DeleteParams, ListParams},
};
use mirrord_operator::crd::mysql_branching::MysqlBranchDatabase;
use prettytable::{Table, row};

use crate::{
    CliResult,
    config::{DbBranchesArgs, DbBranchesCommand},
};

pub async fn db_branches_command(args: DbBranchesArgs) -> CliResult<()> {
    match &args.command {
        DbBranchesCommand::Status { names } => status_command(&args, names.clone()).await,
        DbBranchesCommand::Destroy { all, names } => {
            destroy_command(&args, *all, names.clone()).await
        }
    }
}

async fn status_command(args: &DbBranchesArgs, names: Vec<String>) -> CliResult<()> {
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
                    .map_or(false, |name| names.contains(name))
            })
            .collect()
    };

    if filtered_branches.is_empty() {
        println!("No active DB branch found");
        return Ok(());
    }

    let mut table = Table::new();
    table.add_row(row![
        "Name",
        "DB Type",
        "Phase",
        "TTL (sec)",
        "Database",
        "Users"
    ]);

    for branch in filtered_branches {
        let name = branch.metadata.name.as_deref().unwrap_or("<unknown>");
        let db_type = "MySQL";
        let phase = branch
            .status
            .as_ref()
            .map(|s| s.phase.to_string())
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
                    s.session_info
                        .values()
                        .map(|session| {
                            format!("{}@{}", session.owner.username, session.owner.hostname)
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            })
            .unwrap_or_else(|| "none".to_string());

        table.add_row(row![name, db_type, phase, ttl, db_name, users]);
    }

    table.printstd();

    Ok(())
}

async fn destroy_command(args: &DbBranchesArgs, all: bool, names: Vec<String>) -> CliResult<()> {
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
            return Err(crate::CliError::DbBranchNotFound(
                "No active DB branch found".to_string(),
            ));
        }

        // Delete all branches
        let delete_params = DeleteParams::default();
        for branch in branches {
            if let Some(name) = branch.metadata.name {
                match api.delete(&name, &delete_params).await {
                    Ok(_) => println!("Destroyed branch: {}", name),
                    Err(e) => eprintln!("Failed to destroy branch {}: {}", name, e),
                }
            }
        }
    } else {
        // Check if the specified branches exist
        let mut found_any = false;
        let delete_params = DeleteParams::default();

        for name in names {
            match api.get(&name).await {
                Ok(_) => {
                    found_any = true;
                    match api.delete(&name, &delete_params).await {
                        Ok(_) => println!("Destroyed branch: {}", name),
                        Err(e) => eprintln!("Failed to destroy branch {}: {}", name, e),
                    }
                }
                Err(kube::Error::Api(api_err)) if api_err.code == 404 => {
                    eprintln!("Branch not found: {}", name);
                }
                Err(e) => {
                    return Err(crate::CliError::ListTargetsFailed(
                        mirrord_kube::error::KubeApiError::KubeError(e),
                    ));
                }
            }
        }

        if !found_any {
            return Err(crate::CliError::DbBranchNotFound(
                "No active DB branch found".to_string(),
            ));
        }
    }

    Ok(())
}
