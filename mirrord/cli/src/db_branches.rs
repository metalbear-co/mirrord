use std::collections::HashSet;

use kube::{
    Api,
    api::{DeleteParams, ListParams},
    client::ClientBuilder,
};
use mirrord_config::{LayerConfig, config::ConfigContext};
use mirrord_kube::{api::kubernetes::create_kube_config, retry::RetryKube};
use mirrord_operator::crd::{mysql_branching::MysqlBranchDatabase, pg_branching::PgBranchDatabase};
use mirrord_progress::{Progress, ProgressTracker};
use prettytable::{Table, row};
use tower::{buffer::BufferLayer, retry::RetryLayer};

use crate::{
    CliError, CliResult,
    config::{DbBranchesArgs, DbBranchesCommand},
};

pub async fn db_branches_command(args: DbBranchesArgs) -> CliResult<()> {
    match &args.command {
        DbBranchesCommand::Status { names } => status_command(&args, names.as_slice()).await,
        DbBranchesCommand::Destroy { all, names } => destroy_command(&args, *all, names).await,
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

    let client = create_kube_config(
        layer_config.accept_invalid_certificates,
        layer_config.kubeconfig.clone(),
        layer_config.kube_context.clone(),
    )
    .await
    .and_then(|config| {
        Ok(ClientBuilder::try_from(config.clone())?
            .with_layer(&BufferLayer::new(1024))
            .with_layer(&RetryLayer::new(RetryKube::try_from(
                &layer_config.startup_retry,
            )?))
            .build())
    })
    .map_err(|error| CliError::friendlier_error_or_else(error, CliError::CreateKubeApiFailed))?;

    // Fetch MySQL branches
    let mysql_api: Api<MysqlBranchDatabase> = if args.all_namespaces {
        Api::all(client.clone())
    } else if let Some(namespace) = &args.namespace {
        Api::namespaced(client.clone(), namespace)
    } else if let Some(namespace) = &layer_config.target.namespace {
        Api::namespaced(client.clone(), namespace)
    } else {
        Api::default_namespaced(client.clone())
    };

    // Fetch PostgreSQL branches
    let pg_api: Api<PgBranchDatabase> = if args.all_namespaces {
        Api::all(client.clone())
    } else if let Some(namespace) = &args.namespace {
        Api::namespaced(client.clone(), namespace)
    } else if let Some(namespace) = &layer_config.target.namespace {
        Api::namespaced(client, namespace)
    } else {
        Api::default_namespaced(client)
    };

    let list_params = ListParams::default();

    let mysql_branches = mysql_api.list(&list_params).await.map_err(|e| {
        status_progress.failure(Some("failed to list MySQL branches"));
        crate::CliError::ListTargetsFailed(mirrord_kube::error::KubeApiError::KubeError(e))
    })?;

    let pg_branches = pg_api.list(&list_params).await.map_err(|e| {
        status_progress.failure(Some("failed to list PostgreSQL branches"));
        crate::CliError::ListTargetsFailed(mirrord_kube::error::KubeApiError::KubeError(e))
    })?;

    // Create a unified view with database type information
    #[derive(Debug)]
    struct BranchInfo {
        name: String,
        db_type: &'static str,
        phase: String,
        ttl: u64,
        database: String,
        users: String,
        expire_time: String,
    }

    let mut all_branches = Vec::new();

    // Process MySQL branches
    for branch in mysql_branches {
        if let Some(name) = &branch.metadata.name
            && (names.is_empty() || names.contains(name))
        {
            all_branches.push(BranchInfo {
                name: name.clone(),
                db_type: "MySQL",
                phase: branch
                    .status
                    .as_ref()
                    .map(|s| s.phase.to_string())
                    .unwrap_or_else(|| "Unknown".to_string()),
                ttl: branch.spec.ttl_secs,
                database: branch
                    .spec
                    .database_name
                    .unwrap_or_else(|| "<none>".to_string()),
                users: branch
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
                    .unwrap_or_else(|| "none".to_string()),
                expire_time: branch
                    .status
                    .as_ref()
                    .map(|s| s.expire_time.0.to_rfc3339())
                    .unwrap_or_else(|| "Unknown".to_string()),
            });
        }
    }

    // Process PostgreSQL branches
    for branch in pg_branches {
        if let Some(name) = &branch.metadata.name
            && (names.is_empty() || names.contains(name))
        {
            all_branches.push(BranchInfo {
                name: name.clone(),
                db_type: "PostgreSQL",
                phase: branch
                    .status
                    .as_ref()
                    .map(|s| s.phase.to_string())
                    .unwrap_or_else(|| "Unknown".to_string()),
                ttl: branch.spec.ttl_secs,
                database: branch
                    .spec
                    .database_name
                    .unwrap_or_else(|| "<none>".to_string()),
                users: branch
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
                    .unwrap_or_else(|| "none".to_string()),
                expire_time: branch
                    .status
                    .as_ref()
                    .map(|s| s.expire_time.0.to_rfc3339())
                    .unwrap_or_else(|| "Unknown".to_string()),
            });
        }
    }

    if all_branches.is_empty() {
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

    for branch in all_branches {
        table.add_row(row![
            branch.name,
            branch.db_type,
            branch.phase,
            branch.ttl,
            branch.database,
            branch.users,
            branch.expire_time
        ]);
    }

    table.printstd();

    Ok(())
}

async fn destroy_command(args: &DbBranchesArgs, all: bool, names: &Vec<String>) -> CliResult<()> {
    let mut progress = ProgressTracker::from_env("DB Branches Destroy");
    let mut destroy_progress = progress.subtask("deleting branches");

    let mut cfg_context = ConfigContext::default()
        .override_env_opt(LayerConfig::FILE_PATH_ENV, args.config_file.clone())
        .override_env_opt("MIRRORD_TARGET_NAMESPACE", args.namespace.clone());

    let layer_config = LayerConfig::resolve(&mut cfg_context)?;

    let client = create_kube_config(
        layer_config.accept_invalid_certificates,
        layer_config.kubeconfig.clone(),
        layer_config.kube_context.clone(),
    )
    .await
    .and_then(|config| {
        Ok(ClientBuilder::try_from(config.clone())?
            .with_layer(&BufferLayer::new(1024))
            .with_layer(&RetryLayer::new(RetryKube::try_from(
                &layer_config.startup_retry,
            )?))
            .build())
    })
    .map_err(|error| CliError::friendlier_error_or_else(error, CliError::CreateKubeApiFailed))?;

    let mysql_api: Api<MysqlBranchDatabase> = if args.all_namespaces {
        Api::all(client.clone())
    } else if let Some(namespace) = &args.namespace {
        Api::namespaced(client.clone(), namespace)
    } else if let Some(namespace) = &layer_config.target.namespace {
        Api::namespaced(client.clone(), namespace)
    } else {
        Api::default_namespaced(client.clone())
    };

    let pg_api: Api<PgBranchDatabase> = if args.all_namespaces {
        Api::all(client.clone())
    } else if let Some(namespace) = &args.namespace {
        Api::namespaced(client.clone(), namespace)
    } else if let Some(namespace) = &layer_config.target.namespace {
        Api::namespaced(client, namespace)
    } else {
        Api::default_namespaced(client)
    };

    if all {
        // List all branches first to check if any exist
        let mysql_branches = mysql_api.list(&ListParams::default()).await.map_err(|e| {
            crate::CliError::ListTargetsFailed(mirrord_kube::error::KubeApiError::KubeError(e))
        })?;

        let pg_branches = pg_api.list(&ListParams::default()).await.map_err(|e| {
            crate::CliError::ListTargetsFailed(mirrord_kube::error::KubeApiError::KubeError(e))
        })?;

        if mysql_branches.items.is_empty() && pg_branches.items.is_empty() {
            destroy_progress.success(Some("No active DB branch found."));
        }

        // Delete all MySQL branches
        let delete_params = DeleteParams::default();
        for branch in mysql_branches {
            if let Some(name) = branch.metadata.name {
                let mut branch_progress =
                    destroy_progress.subtask(&format!("destroying MySQL branch {name}"));
                match mysql_api.delete(&name, &delete_params).await {
                    Ok(_) => branch_progress.success(Some(&format!("destroyed {name}"))),
                    Err(e) => branch_progress.failure(Some(&format!("failed: {e}"))),
                }
            }
        }

        // Delete all PostgreSQL branches
        for branch in pg_branches {
            if let Some(name) = branch.metadata.name {
                let mut branch_progress =
                    destroy_progress.subtask(&format!("destroying PostgreSQL branch {name}"));
                match pg_api.delete(&name, &delete_params).await {
                    Ok(_) => branch_progress.success(Some(&format!("destroyed {name}"))),
                    Err(e) => branch_progress.failure(Some(&format!("failed: {e}"))),
                }
            }
        }
        destroy_progress.success(None);
    } else {
        // First, list all branches to determine their types
        let mysql_branches = mysql_api.list(&ListParams::default()).await.map_err(|e| {
            crate::CliError::ListTargetsFailed(mirrord_kube::error::KubeApiError::KubeError(e))
        })?;

        let pg_branches = pg_api.list(&ListParams::default()).await.map_err(|e| {
            crate::CliError::ListTargetsFailed(mirrord_kube::error::KubeApiError::KubeError(e))
        })?;

        // Build sets of names by type for efficient lookup
        let mysql_names: HashSet<String> = mysql_branches
            .into_iter()
            .filter_map(|b| b.metadata.name)
            .collect();

        let pg_names: HashSet<String> = pg_branches
            .into_iter()
            .filter_map(|b| b.metadata.name)
            .collect();

        let delete_params = DeleteParams::default();

        for name in names {
            let mut branch_progress = destroy_progress.subtask(&format!("destroying {name}"));

            // Check which type this branch is and delete from the correct API
            if mysql_names.contains(name) {
                match mysql_api.delete(name, &delete_params).await {
                    Ok(_) => {
                        branch_progress.success(Some(&format!("destroyed MySQL branch: {name}")))
                    }
                    Err(e) => branch_progress
                        .failure(Some(&format!("failed to destroy MySQL branch {name}: {e}"))),
                }
            } else if pg_names.contains(name) {
                match pg_api.delete(name, &delete_params).await {
                    Ok(_) => branch_progress
                        .success(Some(&format!("destroyed PostgreSQL branch: {name}"))),
                    Err(e) => branch_progress.failure(Some(&format!(
                        "failed to destroy PostgreSQL branch {name}: {e}"
                    ))),
                }
            } else {
                branch_progress.failure(Some(&format!("branch not found: {name}")));
            }
        }
        destroy_progress.success(None);
    }

    progress.success(None);

    Ok(())
}
