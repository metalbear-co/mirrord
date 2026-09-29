use std::{
    collections::{HashMap, HashSet},
    time::{Duration, SystemTime},
};

use kube::{Api, api::ListParams};
use mirrord_analytics::NullReporter;
use mirrord_config::{LayerConfig, config::ConfigContext};
use mirrord_operator::{
    client::{MaybeClientCert, NoClientCert, OperatorApi, error::OperatorOperation},
    crd::{Session as OperatorStatusSession, SessionCrd, escape_field_selector_value},
};
use mirrord_progress::{NullProgress, messages::AGENT_OPERATOR_HINT};
use mirrord_session_monitor_client::{
    SessionConnection, connect_to_session, session_endpoints, sessions_dir,
};
use mirrord_session_monitor_protocol::{ProcessInfo, SessionInfo};
use prettytable::{Table, row};
use serde::Serialize;
use tracing::Level;

use crate::{
    config::{
        KillArgs, LocalSessionCommand, SessionArgs, SessionCommonArgs, SessionDeleteArgs,
        SessionListArgs, SessionListFormat,
    },
    error::CliError,
    util::remove_proxy_env,
};

const NOT_AVAILABLE: &str = "N/A";

struct MergedSessionRow {
    session_id: String,
    local: Option<SessionInfo>,
    remote: Option<OperatorStatusSession>,
}

#[derive(Serialize)]
struct JsonSessionRow<'a> {
    session_id: &'a str,
    process_id: Option<u32>,
    key: Option<&'a str>,
    target: Option<&'a str>,
    namespace: Option<&'a str>,
    user: Option<&'a str>,
    process_name: Option<&'a str>,
    command_line: Option<String>,
    time_up: Option<String>,
}

impl<'a> From<&'a MergedSessionRow> for JsonSessionRow<'a> {
    fn from(row: &'a MergedSessionRow) -> Self {
        let process = row.local.as_ref().and_then(primary_process);

        Self {
            session_id: &row.session_id,
            process_id: process.map(|process| process.pid),
            key: row
                .local
                .as_ref()
                .and_then(|session| session.key.as_deref())
                .or_else(|| {
                    row.remote
                        .as_ref()
                        .and_then(|session| session.key.as_deref())
                }),
            target: row.target_value(),
            namespace: row.namespace_value(),
            user: row.remote.as_ref().map(|session| session.user.as_str()),
            process_name: process.map(|process| process.process_name.as_str()),
            command_line: process.map(|process| format_cmdline(Some(process))),
            time_up: row.time_up_value(),
        }
    }
}

enum RemoteKillResult {
    Killed,
    NotFound,
    Unavailable,
}

#[tracing::instrument(level = Level::TRACE, ret, skip_all)]
pub async fn session_command(args: SessionArgs) -> Result<(), CliError> {
    let SessionArgs { common, command } = args;

    match command.unwrap_or_else(LocalSessionCommand::default) {
        LocalSessionCommand::List(args) => list_command(&common, args).await,
        LocalSessionCommand::Stop(args) => delete_command(&common, args).await,
    }
}

#[tracing::instrument(level = Level::TRACE, ret, skip_all)]
pub async fn kill_command(args: KillArgs) -> Result<(), CliError> {
    delete_command(&args.common, args.delete).await
}

#[tracing::instrument(level = Level::TRACE, ret, skip_all)]
async fn list_command(common: &SessionCommonArgs, args: SessionListArgs) -> Result<(), CliError> {
    let (rows, operator_not_found) = merged_sessions(common, &args).await?;

    if args.format == SessionListFormat::Json {
        let rows: Vec<_> = rows.iter().map(JsonSessionRow::from).collect();
        println!("{}", serde_json::to_string(&rows)?);
        return Ok(());
    }

    if operator_not_found {
        println!(
            "Operator not found, showing local sessions only. Get started with operator at app.metalbear.com/?utm_source=sessions-list&utm_medium=cli\n\
             {AGENT_OPERATOR_HINT}"
        );
    }

    if rows.is_empty() {
        println!("No active sessions.");
        return Ok(());
    }

    let mut table = Table::new();
    table.add_row(row![
        "Session ID",
        "Process ID",
        "Key",
        "Target",
        "Namespace",
        "User",
        "Process name",
        "Command line",
        "Time up"
    ]);

    for row in rows {
        let process = row.local.as_ref().and_then(primary_process);
        table.add_row(row![
            row.session_id,
            process
                .map(|process| process.pid.to_string())
                .unwrap_or_else(|| NOT_AVAILABLE.to_owned()),
            row.local
                .as_ref()
                .and_then(|session| session.key.as_deref())
                .unwrap_or(NOT_AVAILABLE),
            row.target_value().unwrap_or(NOT_AVAILABLE),
            row.namespace_value().unwrap_or(NOT_AVAILABLE),
            row.user_value().unwrap_or_else(|| NOT_AVAILABLE.to_owned()),
            process
                .map(|process| process.process_name.as_str())
                .unwrap_or(NOT_AVAILABLE),
            format_cmdline(process),
            row.time_up_value().unwrap_or_else(|| "unknown".to_owned())
        ]);
    }

    table.printstd();

    Ok(())
}

#[tracing::instrument(level = Level::TRACE, ret, skip_all)]
async fn delete_command(
    common: &SessionCommonArgs,
    args: SessionDeleteArgs,
) -> Result<(), CliError> {
    let sessions = load_sessions().await?;

    if let Some(id) = args.id {
        let local_session = sessions
            .into_iter()
            .find(|session| session.info.session_id == id);

        kill_local_then_remote(common, local_session, &id, args.key.as_deref()).await?;
        println!("Killed session {id}.");

        return Ok(());
    }

    let key = args
        .key
        .expect("clap enforces that either id or key is provided");

    let (selected_sessions, deleted_ids): (Vec<_>, Vec<_>) = sessions
        .into_iter()
        .filter_map(|session| {
            (session.info.key.as_deref() == Some(key.as_str())).then(|| {
                let id = session.info.session_id.clone();
                (session, id)
            })
        })
        .unzip();

    if deleted_ids.is_empty() {
        return Err(CliError::Session(format!(
            "no local sessions found with key `{key}`"
        )));
    }

    for (session, session_id) in std::iter::zip(selected_sessions, &deleted_ids) {
        kill_local_then_remote(common, Some(session), session_id, Some(&key)).await?;
    }

    match &deleted_ids[..] {
        [single] => println!("Killed session {single}."),
        _ => println!(
            "Killed {} sessions: {}.",
            deleted_ids.len(),
            deleted_ids.join(", ")
        ),
    }

    Ok(())
}

async fn merged_sessions(
    common: &SessionCommonArgs,
    args: &SessionListArgs,
) -> Result<(Vec<MergedSessionRow>, bool), CliError> {
    let local_sessions = load_sessions().await?;
    let remote_result = try_load_remote_sessions(common, args.key.as_deref()).await;
    let operator_not_found = remote_result.is_none();
    let remote_sessions = remote_result.unwrap_or_default();

    let mut rows: HashMap<String, MergedSessionRow> = HashMap::new();

    for session in local_sessions {
        if args.key.is_some() && session.info.key != args.key {
            continue;
        }

        rows.insert(
            session.info.session_id.clone(),
            MergedSessionRow {
                session_id: session.info.session_id.clone(),
                local: Some(session.info),
                remote: None,
            },
        );
    }

    for session in remote_sessions {
        // Client-side filter for operator versions that don't support the spec.session.key field
        // selector.
        if args.key.is_some() && session.key != args.key {
            continue;
        }

        let session_id = session
            .id
            .clone()
            .unwrap_or_else(|| NOT_AVAILABLE.to_owned());

        rows.entry(session_id.clone())
            .and_modify(|row| row.remote = Some(session.clone()))
            .or_insert(MergedSessionRow {
                session_id,
                local: None,
                remote: Some(session),
            });
    }

    let mut rows: Vec<_> = rows.into_values().collect();
    rows.sort_by_key(|left| (left.local.is_none(), left.sort_key()));

    Ok((rows, operator_not_found))
}

async fn load_sessions() -> Result<Vec<SessionConnection>, CliError> {
    let sessions_dir = sessions_dir()
        .ok_or_else(|| CliError::Session("could not determine home directory".to_owned()))?;
    let mut sessions = Vec::new();

    for (session_id, endpoint) in session_endpoints(&sessions_dir) {
        match connect_to_session(&endpoint.sentinel_path).await {
            Ok(connection) => sessions.push(connection),
            Err(error) => {
                tracing::debug!(%session_id, ?error, "Failed to load local session, removing stale sentinel");
                let _ = std::fs::remove_file(&endpoint.sentinel_path);
            }
        }
    }

    Ok(sessions)
}

/// Returns `None` if the operator is not found (signals operator-not-found to callers),
/// or `Some` with whatever sessions were loaded (empty on other errors).
async fn try_load_remote_sessions(
    common: &SessionCommonArgs,
    key: Option<&str>,
) -> Option<Vec<OperatorStatusSession>> {
    match load_remote_sessions(common, key).await {
        Ok(sessions) => Some(sessions),
        Err(CliError::OperatorNotInstalled) => None,
        Err(error) => {
            tracing::debug!(?error, "Failed to load remote operator sessions");
            Some(Vec::new())
        }
    }
}

async fn load_remote_sessions(
    common: &SessionCommonArgs,
    key: Option<&str>,
) -> Result<Vec<OperatorStatusSession>, CliError> {
    let layer_config = resolve_layer_config(common).await?;

    if !layer_config.use_proxy {
        remove_proxy_env();
    }

    let current_namespace = match (common.all_namespaces, &layer_config.target.namespace) {
        (true, _) => None,
        (false, Some(namespace)) => Some(namespace.clone()),
        (false, None) => Some(
            crate::kube::kube_client_from_layer_config(&layer_config)
                .await?
                .default_namespace()
                .to_owned(),
        ),
    };

    let progress = NullProgress {};
    let api = match OperatorApi::<NoClientCert>::try_new(
        &layer_config,
        &mut NullReporter::default(),
        &progress,
    )
    .await?
    {
        Some(api) => api,
        None => return Ok(Vec::new()),
    };

    let sessions = match list_active_sessions(&api, current_namespace.as_deref(), key).await {
        Ok(sessions) => sessions,
        // Match on the HTTP code, not `Status::is_not_found`/`is_forbidden`, as they rely on the
        // `reason` string, which is not available for an empty body `404` from an un-upgraded
        // operator.
        Err(kube::Error::Api(status)) if status.code == 403 || status.code == 404 => {
            tracing::debug!(
                code = status.code,
                "active-sessions API unavailable, falling back to operator status"
            );

            list_active_sessions_fallback(&api, current_namespace.as_deref(), key)?
        }
        Err(error) => {
            return Err(CliError::OperatorApiFailed(
                OperatorOperation::SessionManagement,
                error,
            ));
        }
    };

    // Preview-env entries are folded into the operator's session list so the local mirrord
    // UI and browser extension can surface them, but they're not real exec sessions and
    // don't behave like ones (different id shape, no locked ports, no queue-splitting
    // state), so we hide them from `mirrord session` to avoid confusing users into running
    // e.g. `mirrord session stop` against a preview.
    Ok(sessions
        .into_iter()
        .filter(|session| !session.is_preview())
        .collect())
}

async fn list_active_sessions(
    api: &OperatorApi<NoClientCert>,
    namespace: Option<&str>,
    key: Option<&str>,
) -> Result<Vec<OperatorStatusSession>, kube::Error> {
    let session_api: Api<SessionCrd> = match namespace {
        Some(namespace) => Api::namespaced(api.client().clone(), namespace),
        None => Api::all(api.client().clone()),
    };

    let list_params = ListParams {
        field_selector: key
            .map(|key| format!("spec.session.key={}", escape_field_selector_value(key))),
        ..Default::default()
    };

    Ok(session_api
        .list(&list_params)
        .await?
        .into_iter()
        .map(|session| session.spec.session)
        .collect())
}

fn list_active_sessions_fallback(
    api: &OperatorApi<NoClientCert>,
    namespace: Option<&str>,
    key: Option<&str>,
) -> Result<Vec<OperatorStatusSession>, CliError> {
    Ok(api
        .operator()
        .status
        .clone()
        .ok_or(CliError::OperatorStatusNotFound)?
        .sessions
        .into_iter()
        .filter(|session| {
            namespace.is_none_or(|namespace| session.namespace.as_deref() == Some(namespace))
        })
        .filter(|session| {
            key.map(|key| session.key.as_deref() == Some(key))
                .unwrap_or(true)
        })
        .collect())
}

async fn kill_local_then_remote(
    common: &SessionCommonArgs,
    local_session: Option<SessionConnection>,
    session_id: &str,
    key: Option<&str>,
) -> Result<(), CliError> {
    let local_killed = if let Some(session) = local_session {
        session.client.kill().await.map_err(|error| {
            CliError::Session(format!(
                "failed to kill local session `{}`: {error}",
                session.info.session_id
            ))
        })?;
        true
    } else {
        false
    };

    match try_kill_remote_session(common, session_id, key).await {
        Ok(RemoteKillResult::Killed) => Ok(()),
        Ok(RemoteKillResult::NotFound | RemoteKillResult::Unavailable) if local_killed => Ok(()),
        Ok(RemoteKillResult::NotFound | RemoteKillResult::Unavailable) => Err(CliError::Session(
            format!("no local or remote session found with id `{session_id}`"),
        )),
        Err(error) if local_killed => {
            tracing::debug!(?error, %session_id, "Failed to kill remote session after killing local session");
            Ok(())
        }
        Err(error) => Err(error),
    }
}

async fn try_kill_remote_session(
    common: &SessionCommonArgs,
    session_id: &str,
    key: Option<&str>,
) -> Result<RemoteKillResult, CliError> {
    let alternate_id = alternate_remote_session_id(session_id);
    let session_ids = match load_remote_sessions(common, key).await {
        Ok(remote_sessions) => {
            let matching_session = remote_sessions.into_iter().find(|session| {
                session
                    .id
                    .as_deref()
                    .is_some_and(|id| id == session_id || alternate_id.as_deref() == Some(id))
            });

            let Some(remote_session_id) = matching_session.and_then(|session| session.id) else {
                return Ok(RemoteKillResult::NotFound);
            };

            vec![remote_session_id]
        }
        Err(CliError::OperatorStatusNotFound) => {
            let mut session_ids = vec![session_id.to_owned()];

            if let Some(alternate_id) = alternate_id {
                session_ids.push(alternate_id);
            }

            session_ids
        }
        Err(CliError::OperatorNotInstalled) => return Ok(RemoteKillResult::Unavailable),
        Err(error) => return Err(error),
    };

    let operator_api = match operator_api_with_client_certificate(common).await? {
        Some(api) => api,
        None => return Ok(RemoteKillResult::Unavailable),
    };

    let session_api: Api<SessionCrd> = Api::all(operator_api.client().clone());

    for session_id in session_ids {
        if delete_remote_session_with_name(&session_api, &session_id).await? {
            return Ok(RemoteKillResult::Killed);
        }
    }

    Ok(RemoteKillResult::NotFound)
}

async fn operator_api_with_client_certificate(
    args: &SessionCommonArgs,
) -> Result<Option<OperatorApi<MaybeClientCert>>, CliError> {
    let layer_config = resolve_layer_config(args).await?;

    if !layer_config.use_proxy {
        remove_proxy_env();
    }

    let progress = NullProgress {};
    let api = match OperatorApi::<NoClientCert>::try_new(
        &layer_config,
        &mut NullReporter::default(),
        &progress,
    )
    .await?
    {
        Some(api) => api,
        None => return Ok(None),
    };

    let api = api
        .with_client_certificate(&mut NullReporter::default(), &progress, &layer_config)
        .await;

    api.inspect_cert_error(|error| {
        tracing::debug!(%error, "Failed to prepare user certificate for remote session kill");
    });

    Ok(Some(api))
}

async fn resolve_layer_config(args: &SessionCommonArgs) -> Result<LayerConfig, CliError> {
    let mut cfg_context = ConfigContext::default()
        .override_env_opt(LayerConfig::FILE_PATH_ENV, args.config_file.clone())
        .override_env_opt("MIRRORD_TARGET_NAMESPACE", args.namespace.clone());

    crate::util::resolve_layer_config(&mut cfg_context).await
}

async fn delete_remote_session_with_name(
    session_api: &Api<SessionCrd>,
    session_name: &str,
) -> Result<bool, CliError> {
    match session_api.delete(session_name, &Default::default()).await {
        Ok(_) => Ok(true),
        Err(kube::Error::Api(status)) if status.code == 404 && status.reason.contains("parse") => {
            Err(CliError::Session(
                "remote session management is not supported by this operator".to_owned(),
            ))
        }
        Err(kube::Error::Api(status)) if status.code == 404 => Ok(false),
        Err(error) => Err(CliError::Session(format!(
            "failed to kill remote session `{session_name}`: {error}"
        ))),
    }
}

fn alternate_remote_session_id(session_id: &str) -> Option<String> {
    u64::from_str_radix(session_id, 16)
        .ok()
        .map(|id| id.to_string())
        .filter(|alternate_id| alternate_id != session_id)
}

fn primary_process(session: &SessionInfo) -> Option<&ProcessInfo> {
    let known_pids: HashSet<_> = session
        .processes
        .iter()
        .map(|process| process.pid)
        .collect();

    session
        .processes
        .iter()
        .find(|process| {
            process
                .parent_pid
                .is_none_or(|parent_pid| !known_pids.contains(&parent_pid))
        })
        .or_else(|| session.processes.iter().min_by_key(|process| process.pid))
}

fn format_cmdline(process: Option<&ProcessInfo>) -> String {
    match process {
        Some(process) if !process.cmdline.is_empty() => process.cmdline.join(" "),
        Some(process) => process.process_name.clone(),
        None => NOT_AVAILABLE.to_owned(),
    }
}

fn format_uptime(started_at: &str) -> Option<String> {
    humantime::parse_rfc3339_weak(started_at)
        .ok()
        .and_then(|started_at| SystemTime::now().duration_since(started_at).ok())
        .map(|duration| {
            humantime::format_duration(Duration::from_secs(duration.as_secs())).to_string()
        })
}

impl MergedSessionRow {
    fn sort_key(&self) -> String {
        self.local
            .as_ref()
            .map(|session| session.started_at.clone())
            .unwrap_or_else(|| self.session_id.clone())
    }

    fn target_value(&self) -> Option<&str> {
        self.local
            .as_ref()
            .map(|session| session.target.as_str())
            .or_else(|| self.remote.as_ref().map(|session| session.target.as_str()))
    }

    fn namespace_value(&self) -> Option<&str> {
        self.local
            .as_ref()
            .and_then(|session| session.namespace.as_deref())
            .or_else(|| {
                self.remote
                    .as_ref()
                    .and_then(|session| session.namespace.as_deref())
            })
    }

    fn user_value(&self) -> Option<String> {
        match (&self.local, &self.remote) {
            (Some(_), Some(session)) => Some(format!("You ({})", session.user)),
            (None, Some(session)) => Some(session.user.clone()),
            _ => None,
        }
    }

    fn time_up_value(&self) -> Option<String> {
        if let Some(local) = &self.local {
            return format_uptime(&local.started_at);
        }

        self.remote.as_ref().map(|session| {
            humantime::format_duration(Duration::from_secs(session.duration_secs)).to_string()
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn local_session() -> SessionInfo {
        SessionInfo {
            session_id: "abc123".to_owned(),
            key: Some("dev".to_owned()),
            target: "deployment/api".to_owned(),
            namespace: None,
            context: None,
            started_at: "invalid".to_owned(),
            mirrord_version: "1.0".to_owned(),
            is_operator: true,
            processes: vec![ProcessInfo {
                pid: 42,
                parent_pid: None,
                process_name: "node".to_owned(),
                cmdline: vec!["node".to_owned(), "app.js".to_owned()],
            }],
            port_subscriptions: Vec::new(),
            config: json!({}),
        }
    }

    fn remote_session() -> OperatorStatusSession {
        serde_json::from_value(json!({
            "id": "abc123",
            "duration_secs": 5,
            "user": "alice",
            "target": "deployment/remote",
            "namespace": "default",
            "locked_ports": null,
            "user_id": null,
            "sqs": null,
            "rmq": null,
            "kafka": null,
            "key": "dev"
        }))
        .unwrap()
    }

    fn json_value(row: &MergedSessionRow) -> Value {
        serde_json::to_value(JsonSessionRow::from(row)).unwrap()
    }

    #[test]
    fn local_json_row_uses_typed_values_and_nulls() {
        let row = MergedSessionRow {
            session_id: "abc123".to_owned(),
            local: Some(local_session()),
            remote: None,
        };

        assert_eq!(
            json_value(&row),
            json!({
                "session_id": "abc123",
                "process_id": 42,
                "key": "dev",
                "target": "deployment/api",
                "namespace": null,
                "user": null,
                "process_name": "node",
                "command_line": "node app.js",
                "time_up": null
            })
        );
    }

    #[test]
    fn remote_json_row_has_no_local_process() {
        let row = MergedSessionRow {
            session_id: "abc123".to_owned(),
            local: None,
            remote: Some(remote_session()),
        };

        assert_eq!(
            json_value(&row),
            json!({
                "session_id": "abc123",
                "process_id": null,
                "key": "dev",
                "target": "deployment/remote",
                "namespace": "default",
                "user": "alice",
                "process_name": null,
                "command_line": null,
                "time_up": "5s"
            })
        );
    }

    #[test]
    fn merged_json_row_prefers_local_details() {
        let row = MergedSessionRow {
            session_id: "abc123".to_owned(),
            local: Some(local_session()),
            remote: Some(remote_session()),
        };
        assert_eq!(
            json_value(&row),
            json!({
                "session_id": "abc123",
                "process_id": 42,
                "key": "dev",
                "target": "deployment/api",
                "namespace": "default",
                "user": "alice",
                "process_name": "node",
                "command_line": "node app.js",
                "time_up": null
            })
        );
    }

    #[test]
    fn empty_json_list_is_an_array() {
        let rows: Vec<JsonSessionRow<'_>> = Vec::new();
        assert_eq!(serde_json::to_string(&rows).unwrap(), "[]");
    }
}
