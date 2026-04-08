use std::{collections::HashSet, fmt::Debug, ops::Not, path::PathBuf};

use k8s_openapi::NamespaceResourceScope;
use kube::{Api, Resource, api::DeleteParams};
use mirrord_config::{LayerConfig, config::ConfigContext};
use mirrord_operator::crd::{
    MirrordOperatorCrd, NewOperatorFeature, OPERATOR_STATUS_NAME,
    db_branching::{
        branch_database::BranchDatabase, mongodb::MongodbBranchDatabase,
        mysql::MysqlBranchDatabase, pg::PgBranchDatabase,
    },
};
use mirrord_progress::{Progress, ProgressTracker};
use prettytable::{Table, row};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    CliResult,
    config::{DbBranchesArgs, DbBranchesCommand},
    kube::{kube_client_from_layer_config, list_resource_if_defined},
};

#[derive(Debug, Serialize, Deserialize)]
pub struct Portforward {
    pub db_id: String,
    pub connection_string: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PortforwardSession {
    pub portforwards: Vec<Portforward>,
    pub key: String,
    pub session_id: u64,
}

/// Directory where portforward session files are stored (`~/.mirrord/db_branch_portforwards/`).
pub fn portforward_session_dir() -> PathBuf {
    home::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".mirrord")
        .join("db_branch_portforwards")
}

#[derive(Debug, Clone)]
struct BranchInfo {
    name: String,
    pod_name: Option<String>,
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
            pod_name: status.as_ref().and_then(|s| s.pod_name.clone()),
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
            expire_time: status.as_ref().map(|s| s.expire_time.0.to_string()),
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
            pod_name: status.as_ref().and_then(|s| s.pod_name.clone()),
            db_type: "MySQL".to_string(),
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
            expire_time: status.as_ref().map(|s| s.expire_time.0.to_string()),
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
            pod_name: status.as_ref().and_then(|s| s.pod_name.clone()),
            db_type: "PostgreSQL".to_string(),
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
            expire_time: status.as_ref().map(|s| s.expire_time.0.to_string()),
        }
    }
}

impl From<MongodbBranchDatabase> for BranchInfo {
    fn from(
        MongodbBranchDatabase {
            metadata,
            spec,
            mut status,
        }: MongodbBranchDatabase,
    ) -> Self {
        Self {
            name: metadata.name.unwrap_or_default(),
            pod_name: status.as_ref().and_then(|s| s.pod_name.clone()),
            db_type: "MongoDB".to_string(),
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
            expire_time: status.as_ref().map(|s| s.expire_time.0.to_string()),
        }
    }
}

pub async fn db_branches_command(args: DbBranchesArgs) -> CliResult<()> {
    match &args.command {
        DbBranchesCommand::Status { names } => status_command(&args, names.as_slice()).await,
        DbBranchesCommand::Connections => connections_command().await,
        DbBranchesCommand::Destroy { all, names } => destroy_command(&args, *all, names).await,
    }
}

async fn connections_command() -> CliResult<()> {
    let pf_dir = portforward_session_dir();

    let mut entries = match tokio::fs::read_dir(&pf_dir).await {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            println!("No active portforward sessions.");
            return Ok(());
        }
        Err(err) => {
            tracing::warn!(?err, "failed to read portforward session directory");
            println!("No active portforward sessions.");
            return Ok(());
        }
    };

    let mut table = Table::new();
    table.add_row(row!["DB ID", "ADDRESS", "KEY", "SESSION ID"]);
    let mut has_rows = false;

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();

        let pid: u32 = match path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse().ok())
        {
            Some(pid) => pid,
            None => continue,
        };

        // Check if the process is still alive.
        if is_pid_alive(pid).not() {
            // Stale file, clean it up.
            let _ = tokio::fs::remove_file(&path).await;
            continue;
        }

        let contents = match tokio::fs::read(&path).await {
            Ok(c) => c,
            Err(_) => continue,
        };
        let session: PortforwardSession = match serde_json::from_slice(&contents) {
            Ok(s) => s,
            Err(_) => continue,
        };

        for pf in &session.portforwards {
            table.add_row(row![
                pf.db_id,
                pf.connection_string,
                session.key,
                format!("{:X}", session.session_id)
            ]);
            has_rows = true;
        }
    }

    if has_rows {
        table.printstd();
    } else {
        println!("No active portforward sessions.");
    }

    Ok(())
}

#[cfg(unix)]
fn is_pid_alive(pid: u32) -> bool {
    use nix::{sys::signal::kill, unistd::Pid};

    match kill(Pid::from_raw(pid as i32), None) {
        Ok(()) => true,
        Err(nix::errno::Errno::ESRCH) => false,
        // EPERM means the process exists but we can't signal it.
        Err(_) => true,
    }
}

#[cfg(windows)]
fn is_pid_alive(pid: u32) -> bool {
    // Untested AI slop
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
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

/// Check if the operator supports the unified BranchDatabase CRD by fetching its status
/// and looking for the UnifiedBranchDbCrd feature flag.
async fn operator_supports_unified_crd(client: &kube::Client) -> bool {
    let api: Api<MirrordOperatorCrd> = Api::all(client.clone());
    match api.get(OPERATOR_STATUS_NAME).await {
        Ok(crd) => crd
            .spec
            .supported_features()
            .contains(&NewOperatorFeature::UnifiedBranchDbCrd),
        Err(_) => false,
    }
}

/// Collect branches from per-dialect CRDs (old operator without unified BranchDatabase).
async fn collect_per_dialect_branches<P: Progress>(
    args: &DbBranchesArgs,
    client: &kube::Client,
    layer_config: &LayerConfig,
    progress: &mut P,
) -> CliResult<Vec<BranchInfo>> {
    let mut all = Vec::new();

    let pg_api: Api<PgBranchDatabase> = get_api(args, client, layer_config);
    let mysql_api: Api<MysqlBranchDatabase> = get_api(args, client, layer_config);
    let mongodb_api: Api<MongodbBranchDatabase> = get_api(args, client, layer_config);

    if let Some(pgs) = list_resource_if_defined(&pg_api, progress).await? {
        all.extend(pgs.into_iter().map(BranchInfo::from));
    }
    if let Some(mysqls) = list_resource_if_defined(&mysql_api, progress).await? {
        all.extend(mysqls.into_iter().map(BranchInfo::from));
    }
    if let Some(mongos) = list_resource_if_defined(&mongodb_api, progress).await? {
        all.extend(mongos.into_iter().map(BranchInfo::from));
    }

    Ok(all)
}

async fn status_command(args: &DbBranchesArgs, names: &[String]) -> CliResult<()> {
    let names: HashSet<_> = names.iter().map(|s| s.as_str()).collect();

    let mut progress = ProgressTracker::from_env("DB Branches Status");
    let mut status_progress = progress.subtask("fetching branches");

    let mut cfg_context = ConfigContext::default()
        .override_env_opt(LayerConfig::FILE_PATH_ENV, args.config_file.clone())
        .override_env_opt("MIRRORD_TARGET_NAMESPACE", args.namespace.clone());

    let layer_config = LayerConfig::resolve(&mut cfg_context)?;

    let client = kube_client_from_layer_config(&layer_config).await?;

    let use_unified = operator_supports_unified_crd(&client).await;

    let all_infos: Vec<BranchInfo> = if use_unified {
        let branch_api: Api<BranchDatabase> = get_api(args, &client, &layer_config);
        list_resource_if_defined(&branch_api, &mut status_progress)
            .await?
            .unwrap_or_default()
            .into_iter()
            .map(BranchInfo::from)
            .collect()
    } else {
        collect_per_dialect_branches(args, &client, &layer_config, &mut status_progress).await?
    };

    if all_infos.is_empty() {
        progress.success(Some("No active DB branch found"));
        return Ok(());
    }

    status_progress.success(Some("fetched status"));
    progress.success(None);

    build_status_table(all_infos, names).printstd();
    Ok(())
}

/// Builds a status table from the given branches.
fn build_status_table(all_infos: Vec<BranchInfo>, names: HashSet<&str>) -> Table {
    let all_infos_iter = all_infos
        .into_iter()
        .filter(|b| names.is_empty() || names.contains(b.name.as_str()));

    let mut table = Table::new();
    table.add_row(row![
        "Name",
        "Pod Name",
        "DB Type",
        "Phase",
        "TTL (sec)",
        "Database",
        "Users",
        "Expires At"
    ]);

    for info in all_infos_iter {
        table.add_row(row![
            info.name,
            info.pod_name.unwrap_or_else(|| "Unknown".to_owned()),
            info.db_type,
            info.phase.unwrap_or_else(|| "Unknown".to_owned()),
            info.ttl,
            info.database.unwrap_or_else(|| "<none>".to_owned()),
            info.users.unwrap_or_else(|| "none".to_owned()),
            info.expire_time.unwrap_or_else(|| "Unknown".to_owned())
        ]);
    }

    table
}

/// Delete resources by (name, namespace) pairs, creating a namespaced API for each.
async fn delete_branches<I, R, P>(
    branches: I,
    client: &kube::Client,
    progress: &P,
    delete_params: &DeleteParams,
) where
    I: Iterator<Item = (String, String)>,
    R: Resource<DynamicType = (), Scope = NamespaceResourceScope>
        + Clone
        + DeserializeOwned
        + Debug,
    P: Progress,
{
    for (name, namespace) in branches {
        let mut branch_progress = progress.subtask(&format!("destroying {} {name}", R::kind(&())));
        let api: Api<R> = Api::namespaced(client.clone(), &namespace);
        match api.delete(&name, delete_params).await {
            Ok(_) => branch_progress.success(Some(&format!("destroyed {name}"))),
            Err(e) => branch_progress.failure(Some(&format!("failed: {e}"))),
        }
    }
}

/// Extract (name, namespace) from a resource's metadata, using a fallback namespace
/// for resources that don't have one set (e.g. listed from default namespace).
fn name_and_ns<R: Resource>(resource: &R, fallback_ns: &str) -> Option<(String, String)> {
    let name = resource.meta().name.clone()?;
    let ns = resource
        .meta()
        .namespace
        .clone()
        .unwrap_or_else(|| fallback_ns.to_string());
    Some((name, ns))
}

async fn destroy_command(args: &DbBranchesArgs, all: bool, names: &[String]) -> CliResult<()> {
    let mut progress = ProgressTracker::from_env("DB Branches Destroy");
    let mut destroy_progress = progress.subtask("deleting branches");

    let mut cfg_context = ConfigContext::default()
        .override_env_opt(LayerConfig::FILE_PATH_ENV, args.config_file.clone())
        .override_env_opt("MIRRORD_TARGET_NAMESPACE", args.namespace.clone());

    let layer_config = LayerConfig::resolve(&mut cfg_context)?;

    let client = kube_client_from_layer_config(&layer_config).await?;
    let default_ns = layer_config
        .target
        .namespace
        .as_deref()
        .unwrap_or_else(|| client.default_namespace());

    let use_unified = operator_supports_unified_crd(&client).await;
    let d_params = DeleteParams::default();

    if use_unified {
        let branch_api: Api<BranchDatabase> = get_api(args, &client, &layer_config);
        let branches = list_resource_if_defined(&branch_api, &mut destroy_progress)
            .await?
            .unwrap_or_default();

        let all_pairs: Vec<_> = branches
            .iter()
            .filter_map(|b| name_and_ns(b, default_ns))
            .collect();

        if all {
            if all_pairs.is_empty() {
                destroy_progress.success(Some("No active DB branch found."));
            } else {
                delete_branches::<_, BranchDatabase, _>(
                    all_pairs.into_iter(),
                    &client,
                    &destroy_progress,
                    &d_params,
                )
                .await;
                destroy_progress.success(None);
            }
        } else {
            let mut wanted: HashSet<_> = names.iter().collect();
            let matching = all_pairs
                .into_iter()
                .filter(|(name, _)| wanted.remove(name));
            delete_branches::<_, BranchDatabase, _>(
                matching,
                &client,
                &destroy_progress,
                &d_params,
            )
            .await;

            for name in wanted {
                destroy_progress.failure(Some(&format!("branch not found: {name}")));
            }
            destroy_progress.success(None);
        }
    } else {
        let pg_api: Api<PgBranchDatabase> = get_api(args, &client, &layer_config);
        let mysql_api: Api<MysqlBranchDatabase> = get_api(args, &client, &layer_config);
        let mongodb_api: Api<MongodbBranchDatabase> = get_api(args, &client, &layer_config);

        let mysql_branches = list_resource_if_defined(&mysql_api, &mut destroy_progress).await?;
        let pg_branches = list_resource_if_defined(&pg_api, &mut destroy_progress).await?;
        let mongodb_branches =
            list_resource_if_defined(&mongodb_api, &mut destroy_progress).await?;

        let mysql_pairs: Vec<_> = mysql_branches
            .iter()
            .flatten()
            .filter_map(|b| name_and_ns(b, default_ns))
            .collect();
        let pg_pairs: Vec<_> = pg_branches
            .iter()
            .flatten()
            .filter_map(|b| name_and_ns(b, default_ns))
            .collect();
        let mongo_pairs: Vec<_> = mongodb_branches
            .iter()
            .flatten()
            .filter_map(|b| name_and_ns(b, default_ns))
            .collect();

        if all {
            if mysql_pairs.is_empty() && pg_pairs.is_empty() && mongo_pairs.is_empty() {
                destroy_progress.success(Some("No active DB branch found."));
            } else {
                delete_branches::<_, MysqlBranchDatabase, _>(
                    mysql_pairs.into_iter(),
                    &client,
                    &destroy_progress,
                    &d_params,
                )
                .await;
                delete_branches::<_, PgBranchDatabase, _>(
                    pg_pairs.into_iter(),
                    &client,
                    &destroy_progress,
                    &d_params,
                )
                .await;
                delete_branches::<_, MongodbBranchDatabase, _>(
                    mongo_pairs.into_iter(),
                    &client,
                    &destroy_progress,
                    &d_params,
                )
                .await;
                destroy_progress.success(None);
            }
        } else {
            let mut wanted: HashSet<_> = names.iter().collect();

            let mysql_matching = mysql_pairs
                .into_iter()
                .filter(|(name, _)| wanted.remove(name));
            delete_branches::<_, MysqlBranchDatabase, _>(
                mysql_matching,
                &client,
                &destroy_progress,
                &d_params,
            )
            .await;

            let pg_matching = pg_pairs.into_iter().filter(|(name, _)| wanted.remove(name));
            delete_branches::<_, PgBranchDatabase, _>(
                pg_matching,
                &client,
                &destroy_progress,
                &d_params,
            )
            .await;

            let mongo_matching = mongo_pairs
                .into_iter()
                .filter(|(name, _)| wanted.remove(name));
            delete_branches::<_, MongodbBranchDatabase, _>(
                mongo_matching,
                &client,
                &destroy_progress,
                &d_params,
            )
            .await;

            for name in wanted {
                destroy_progress.failure(Some(&format!("branch not found: {name}")));
            }
            destroy_progress.success(None);
        }
    }

    progress.success(None);

    Ok(())
}

#[cfg(test)]
mod tests {
    use prettytable::Row;

    use super::{BranchInfo, HashSet, Table, build_status_table, row};

    fn branch_info(name: &str, db_type: &'static str) -> BranchInfo {
        BranchInfo {
            name: name.to_owned(),
            pod_name: None,
            db_type: db_type.to_string(),
            phase: None,
            ttl: 3600,
            database: None,
            users: None,
            expire_time: None,
        }
    }

    fn build_expected_table(rows: Vec<Row>) -> Table {
        let mut table = Table::from_iter([row![
            "Name",
            "Pod Name",
            "DB Type",
            "Phase",
            "TTL (sec)",
            "Database",
            "Users",
            "Expires At"
        ]]);
        rows.into_iter().for_each(|row| {
            table.add_row(row);
        });
        table
    }

    #[test]
    fn test_build_status_table() {
        let branches = vec![
            branch_info("branch-a", "Db1"),
            branch_info("branch-b", "Db2"),
            branch_info("branch-c", "Db3"),
        ];

        // No names filter - all branches rendered.
        let table = build_status_table(branches.clone(), HashSet::new());
        let expected = build_expected_table(vec![
            row![
                "branch-a", "Unknown", "Db1", "Unknown", "3600", "<none>", "none", "Unknown"
            ],
            row![
                "branch-b", "Unknown", "Db2", "Unknown", "3600", "<none>", "none", "Unknown"
            ],
            row![
                "branch-c", "Unknown", "Db3", "Unknown", "3600", "<none>", "none", "Unknown"
            ],
        ]);
        assert_eq!(
            expected, table,
            "\n\nexpected:\n{}got:\n{}",
            expected, table
        );

        // Names filter - only matching branches rendered.
        let names = HashSet::from(["branch-a", "branch-c"]);
        let table = build_status_table(branches.clone(), names);
        let expected = build_expected_table(vec![
            row![
                "branch-a", "Unknown", "Db1", "Unknown", "3600", "<none>", "none", "Unknown"
            ],
            row![
                "branch-c", "Unknown", "Db3", "Unknown", "3600", "<none>", "none", "Unknown"
            ],
        ]);
        assert_eq!(
            expected, table,
            "\n\nexpected:\n{}got:\n{}",
            expected, table
        );

        // All branches filtered out - None returned.
        let names: HashSet<&str> = ["nonexistent"].into_iter().collect();
        let table = build_status_table(branches.clone(), names);
        let expected = build_expected_table(vec![]);
        assert_eq!(
            expected, table,
            "\n\nexpected:\n{}got:\n{}",
            expected, table
        );
    }
}
