use std::{
    collections::{HashMap, HashSet},
    time::{Duration, SystemTime},
};

use kube::Api;
use mirrord_analytics::NullReporter;
use mirrord_config::{LayerConfig, config::ConfigContext};
use mirrord_intproxy::session_monitor::api::{ProcessInfo, SessionInfo};
use mirrord_operator::{
    client::{MaybeClientCert, NoClientCert, OperatorApi},
    crd::{Session as OperatorStatusSession, SessionCrd},
};
use mirrord_progress::NullProgress;
use mirrord_session_monitor_client::{
    SessionConnection, connect_to_session, kill_session, session_socket_entries, sessions_dir,
};
use prettytable::{Table, row};
use tracing::Level;

use crate::{
    config::{LocalSessionCommand, SessionArgs, SessionDeleteArgs},
    error::CliError,
    util::remove_proxy_env,
};

const NOT_AVAILABLE: &str = "N/A";

struct MergedSessionRow {
    session_id: String,
    local: Option<SessionInfo>,
    remote: Option<OperatorStatusSession>,
}

enum RemoteKillResult {
    Killed,
    NotFound,
    Unavailable,
}

enum RemoteSessionsLoad {
    Loaded(Vec<OperatorStatusSession>),
    OperatorNotFound,
}

#[tracing::instrument(level = Level::TRACE, ret, skip_all)]
pub async fn session_command(args: SessionArgs) -> Result<(), CliError> {
    match args.command.unwrap_or(LocalSessionCommand::List) {
        LocalSessionCommand::List => list_command().await,
        LocalSessionCommand::Delete(args) => delete_command(args).await,
    }
}

#[tracing::instrument(level = Level::TRACE, ret, skip_all)]
async fn list_command() -> Result<(), CliError> {
    let (rows, operator_not_found) = merged_sessions().await?;

    if operator_not_found {
        println!(
            "Operator not found, showing local sessions only. Get started with operator at app.metalbear.com/?utm_source=sessions-list&utm_medium=cli"
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
            row.target(),
            row.namespace(),
            row.user(),
            process
                .map(|process| process.process_name.as_str())
                .unwrap_or(NOT_AVAILABLE),
            format_cmdline(process),
            row.time_up()
        ]);
    }

    table.printstd();

    Ok(())
}

#[tracing::instrument(level = Level::TRACE, ret, skip_all)]
pub async fn delete_command(args: SessionDeleteArgs) -> Result<(), CliError> {
    let sessions = load_sessions().await?;

    if let Some(id) = args.id {
        let local_session = sessions
            .into_iter()
            .find(|session| session.info.session_id == id);

        kill_local_then_remote(local_session, &id).await?;
        println!("Killed session {id}.");

        return Ok(());
    }

    let key = args
        .key
        .expect("clap enforces that either id or key is provided");

    let selected_sessions: Vec<_> = sessions
        .into_iter()
        .filter(|session| session.info.key.as_deref() == Some(key.as_str()))
        .collect();

    if selected_sessions.is_empty() {
        return Err(CliError::UiError(format!(
            "no local sessions found with key `{key}`"
        )));
    }

    let deleted_ids: Vec<_> = selected_sessions
        .iter()
        .map(|session| session.info.session_id.clone())
        .collect();

    for session in selected_sessions {
        let session_id = session.info.session_id.clone();
        kill_local_then_remote(Some(session), &session_id).await?;
    }

    match deleted_ids.len() {
        1 => println!(
            "Killed session {}.",
            deleted_ids
                .first()
                .expect("one deleted session id should exist")
        ),
        count => println!("Killed {count} sessions: {}.", deleted_ids.join(", ")),
    }

    Ok(())
}

async fn merged_sessions() -> Result<(Vec<MergedSessionRow>, bool), CliError> {
    let local_sessions = load_sessions().await?;
    let (remote_sessions, operator_not_found) = match try_load_remote_sessions().await {
        RemoteSessionsLoad::Loaded(sessions) => (sessions, false),
        RemoteSessionsLoad::OperatorNotFound => (Vec::new(), true),
    };

    let mut rows: HashMap<String, MergedSessionRow> = HashMap::new();

    for session in local_sessions {
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
        .ok_or_else(|| CliError::UiError("could not determine home directory".to_owned()))?;
    let mut sessions = Vec::new();

    for (session_id, socket_path) in session_socket_entries(&sessions_dir) {
        match connect_to_session(&socket_path).await {
            Ok(connection) => sessions.push(connection),
            Err(error) => {
                tracing::debug!(%session_id, ?error, "Failed to load local session, removing stale socket");
                let _ = std::fs::remove_file(&socket_path);
            }
        }
    }

    Ok(sessions)
}

async fn try_load_remote_sessions() -> RemoteSessionsLoad {
    match load_remote_sessions().await {
        Ok(sessions) => RemoteSessionsLoad::Loaded(sessions),
        Err(CliError::OperatorNotInstalled) => RemoteSessionsLoad::OperatorNotFound,
        Err(error) => {
            tracing::debug!(?error, "Failed to load remote operator sessions");
            RemoteSessionsLoad::Loaded(Vec::new())
        }
    }
}

async fn load_remote_sessions() -> Result<Vec<OperatorStatusSession>, CliError> {
    let mut cfg_context = ConfigContext::default();
    let layer_config = LayerConfig::resolve(&mut cfg_context)?;

    if !layer_config.use_proxy {
        remove_proxy_env();
    }

    let current_namespace = match &layer_config.target.namespace {
        Some(namespace) => namespace.clone(),
        None => crate::kube::kube_client_from_layer_config(&layer_config)
            .await?
            .default_namespace()
            .to_owned(),
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

    // Workaround until operator sessions are exposed through a namespaced CRD.
    // `mirrord operator status` returns sessions cluster-wide, but `mirrord session`
    // should only surface sessions for the current effective namespace.
    Ok(api
        .operator()
        .status
        .clone()
        .ok_or(CliError::OperatorStatusNotFound)?
        .sessions
        .into_iter()
        .filter(|session| session.namespace.as_deref() == Some(current_namespace.as_str()))
        .collect())
}

async fn kill_local_then_remote(
    local_session: Option<SessionConnection>,
    session_id: &str,
) -> Result<(), CliError> {
    let local_killed = if let Some(session) = local_session {
        kill_session(&session.client).await.map_err(|error| {
            CliError::UiError(format!(
                "failed to kill local session `{}`: {error}",
                session.info.session_id
            ))
        })?;
        true
    } else {
        false
    };

    match try_kill_remote_session(session_id).await {
        Ok(RemoteKillResult::Killed) => Ok(()),
        Ok(RemoteKillResult::NotFound | RemoteKillResult::Unavailable) if local_killed => Ok(()),
        Ok(RemoteKillResult::NotFound | RemoteKillResult::Unavailable) => Err(CliError::UiError(
            format!("no local or remote session found with id `{session_id}`"),
        )),
        Err(error) if local_killed => {
            tracing::debug!(?error, %session_id, "Failed to kill remote session after killing local session");
            Ok(())
        }
        Err(error) => Err(error),
    }
}

async fn try_kill_remote_session(session_id: &str) -> Result<RemoteKillResult, CliError> {
    let operator_api = match operator_api_with_client_certificate().await? {
        Some(api) => api,
        None => return Ok(RemoteKillResult::Unavailable),
    };

    let session_api: Api<SessionCrd> = Api::all(operator_api.client().clone());

    if delete_remote_session_with_name(&session_api, session_id).await? {
        return Ok(RemoteKillResult::Killed);
    }

    if let Some(alternate_id) = alternate_remote_session_id(session_id)
        && delete_remote_session_with_name(&session_api, &alternate_id).await?
    {
        return Ok(RemoteKillResult::Killed);
    }

    Ok(RemoteKillResult::NotFound)
}

async fn operator_api_with_client_certificate()
-> Result<Option<OperatorApi<MaybeClientCert>>, CliError> {
    let mut cfg_context = ConfigContext::default();
    let layer_config = LayerConfig::resolve(&mut cfg_context)?;

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

async fn delete_remote_session_with_name(
    session_api: &Api<SessionCrd>,
    session_name: &str,
) -> Result<bool, CliError> {
    match session_api.delete(session_name, &Default::default()).await {
        Ok(_) => Ok(true),
        Err(kube::Error::Api(status)) if status.code == 404 && status.reason.contains("parse") => {
            Err(CliError::UiError(
                "remote session management is not supported by this operator".to_owned(),
            ))
        }
        Err(kube::Error::Api(status)) if status.code == 404 => Ok(false),
        Err(error) => Err(CliError::UiError(format!(
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

fn format_uptime(started_at: &str) -> String {
    humantime::parse_rfc3339_weak(started_at)
        .ok()
        .and_then(|started_at| SystemTime::now().duration_since(started_at).ok())
        .map(|duration| {
            humantime::format_duration(Duration::from_secs(duration.as_secs())).to_string()
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

impl MergedSessionRow {
    fn sort_key(&self) -> String {
        self.local
            .as_ref()
            .map(|session| session.started_at.clone())
            .unwrap_or_else(|| self.session_id.clone())
    }

    fn target(&self) -> &str {
        self.local
            .as_ref()
            .map(|session| session.target.as_str())
            .or_else(|| self.remote.as_ref().map(|session| session.target.as_str()))
            .unwrap_or(NOT_AVAILABLE)
    }

    fn namespace(&self) -> &str {
        self.local
            .as_ref()
            .and_then(|session| session.namespace.as_deref())
            .or_else(|| {
                self.remote
                    .as_ref()
                    .and_then(|session| session.namespace.as_deref())
            })
            .unwrap_or(NOT_AVAILABLE)
    }

    fn user(&self) -> String {
        match (&self.local, &self.remote) {
            (Some(_), Some(session)) => format!("You ({})", session.user),
            (None, Some(session)) => session.user.clone(),
            _ => NOT_AVAILABLE.to_owned(),
        }
    }

    fn time_up(&self) -> String {
        self.local
            .as_ref()
            .map(|session| format_uptime(&session.started_at))
            .or_else(|| {
                self.remote.as_ref().map(|session| {
                    humantime::format_duration(Duration::from_secs(session.duration_secs))
                        .to_string()
                })
            })
            .unwrap_or_else(|| "unknown".to_owned())
    }
}
