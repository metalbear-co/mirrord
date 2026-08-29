//! # Local mirrord daemon and UI
//!
//! The daemon starts automatically with mirrord sessions and owns local services shared between
//! them. The `mirrord ui` command opens its web-based session monitor. The daemon watches
//! `~/.mirrord/sessions/` for session sentinel files (`.sock` on unix, `.pipe` on windows),
//! connects to each session's HTTP API, and serves REST/SSE/WebSocket endpoints on localhost.
//!
//! It also enables chaos testing by updating chaos rules enforced in the internal proxy.
//!
//! ## mirrord Wizard (aka onboarding Wizard)
//!
//! `mirrord wizard` is a thin alias for `mirrord ui` that opens the browser directly on the config
//! wizard page (`/wizard`).
//!
//! The wizard's frontend and backend endpoints are served by the local daemon (see [`crate::ui`]
//! and `ui::wizard`). This command starts the daemon if needed and points the browser at the wizard
//! page. The frontend itself lives in `packages/ui` (composing `packages/wizard`).

#[cfg(unix)]
use std::num::ParseIntError;
use std::{env, io::Read, str::FromStr};

use futures::future::join_all;
use miette::Diagnostic;
use mirrord_analytics::{AnalyticsReporter, ExecutionKind};
use mirrord_config::util::VecOrSingle;
use mirrord_intproxy::session_monitor::chaos::rules::{ChaosRule, ChaosRuleRequest};
use mirrord_session_monitor_client::{
    Response, SessionClient, SessionError, session_endpoints, sessions_dir,
};
#[cfg(unix)]
use nix::errno::Errno;
use serde_json::{Value, json};
use thiserror::Error;
use tracing::{error, info};

use crate::{
    CliError,
    config::{
        ChaosArgs, ChaosFormat, ChaosSubcommand, UI_DEFAULT_PORT, UiCommonArgs, UiSubcommand,
    },
    error::CliResult,
    ui::chaos::{api::BASE_INTPROXY_CHAOS_ROUTE, error::ChaosApiError},
    user_data::UserData,
};

mod chaos;
mod daemon;
pub(crate) mod db_portforwards;
mod error;
pub mod server;
mod wizard;

const MAX_EVENTS_PER_SESSION: usize = 500;

pub(crate) use daemon::{DaemonClient, ensure_daemon, ui_stop};
use daemon::{
    MIRRORD_SERVER_PORT_ENV_NAME, ServerDetails, TOKEN_HEADER_NAME, stop_daemon, ui_run_server,
    ui_start,
};

#[derive(Debug, Error, Diagnostic)]
pub enum UiCliError {
    #[error("the local mirrord daemon process failed: {0}")]
    UiServer(#[from] UiServerError),

    /// IO error for the foreground process - for the daemon, use [`UiServerError::Io`].
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// May occur while trying to kill the existing local daemon.
    #[cfg(unix)]
    #[error("failed to perform an operation on the local mirrord daemon process: {0}")]
    #[diagnostic(help(
        "To forcefully stop the daemon process, try killing it manually. On \
        unix for example, find the process with `ps aux | grep mirrord ui` and \
        then `kill $PID` to stop it running."
    ))]
    Process(#[from] Errno),

    /// Occurs when the foreground task is waiting to read an "OK" message from the stdout of the
    /// child, and it times out or gets a different response or error.
    #[error("failed to communicate with the daemon background process: {0}")]
    SpawnBackgroundTask(String),

    /// May occur when the foreground task cannot get the child task's PID, indicating that the
    /// process has exited.
    #[error("the new daemon process ended unexpectedly")]
    ChildExitedUnexpectedly,

    #[cfg(unix)]
    #[error("failed to parse a PID from file contents: {0}")]
    PidParse(ParseIntError),

    #[error("couldn't kill the local mirrord daemon because its PID file was not found")]
    #[diagnostic(help(
        "Try killing the process manually. On Unix, for example, run `ps aux | grep mirrord` and \
        then `kill $PID` in a terminal."
    ))]
    MissingPidFile,

    #[error("failed to communicate with the local mirrord daemon: {0}")]
    DaemonRequest(#[from] reqwest::Error),

    #[error("local mirrord daemon returned an invalid response: {0}")]
    DaemonResponse(String),

    #[error(
        "cannot stop the local mirrord daemon because these sessions still use shared DB port forwards: {0}. Stop them and retry"
    )]
    DaemonInUse(String),

    /// Errors from making requests to the session monitor in `mirrord chaos`
    #[error(transparent)]
    Chaos(#[from] ChaosApiError),
}

impl From<SessionError> for UiCliError {
    fn from(value: SessionError) -> Self {
        ChaosApiError::SessionMonitor(value).into()
    }
}

impl From<serde_json::Error> for UiCliError {
    fn from(value: serde_json::Error) -> Self {
        SessionError::Json(value).into()
    }
}

#[derive(Debug, Error, Diagnostic)]
pub enum UiServerError {
    /// IO error for the background process.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("failed to create watcher: {0}")]
    Watcher(#[from] notify::Error),
}

/// Prints daemon and optional Web UI details to the foreground task's `stdout`.
fn ui_start_printout(
    ServerDetails {
        already_running,
        url,
        token,
        server_pid,
        std_err_file,
    }: &ServerDetails,
) {
    let mut lines = String::new();

    lines.push('\n');
    if *already_running {
        lines.push_str("* Local mirrord daemon already running\n");
    } else {
        lines.push_str("* New local mirrord daemon started\n");
    }
    lines.push_str("* Daemon PID:\n");
    lines.push_str(format!(" -> {server_pid}\n").as_str());

    lines.push('\n');
    lines.push_str("* Web UI:\n");
    lines.push_str(format!(" -> {url}\n").as_str());
    lines.push_str("* API token:\n");
    lines.push_str(format!(" -> {TOKEN_HEADER_NAME}: {token}\n").as_str());

    lines.push('\n');
    if *already_running {
        lines.push_str("* mirrord session monitor ready!\n");
    } else {
        lines.push_str("* mirrord session monitor ready!\n");
        lines.push_str(
            format!(" -> daemon log file: {}\n", std_err_file.to_string_lossy()).as_str(),
        );
    }

    println!("{lines}")
}

/// Runs the `mirrord ui` command. Starting opens the daemon's UI; stopping terminates the daemon
/// when no sessions still depend on its shared DB forwards. Failed startup performs private daemon
/// cleanup.
///
/// `open_path` selects which page the browser opens on when the daemon starts (`/` for the session
/// monitor, `/wizard` for the config wizard). It has no effect on [`UiSubcommand::Stop`].
pub async fn ui_command(
    UiCommonArgs { port, no_browser }: UiCommonArgs,
    command: Option<UiSubcommand>,
    open_path: &str,
) -> Result<(), UiCliError> {
    match command.unwrap_or(UiSubcommand::Start) {
        UiSubcommand::Start => {
            if let Ok(port) = env::var(MIRRORD_SERVER_PORT_ENV_NAME) {
                ui_run_server(u16::from_str(&port).unwrap_or(UI_DEFAULT_PORT)).await?;
                Ok(())
            } else {
                match ui_start(port, no_browser, open_path).await {
                    Ok(details) => {
                        ui_start_printout(&details);
                        Ok(())
                    }
                    Err(error) => {
                        error!("`mirrord ui` failed to start the daemon, cleaning up startup");
                        let _ = stop_daemon(false).await;
                        Err(error)
                    }
                }
            }
        }
        UiSubcommand::Stop => ui_stop(true).await,
    }
}

/// The entrypoint for the `wizard` command. Starts the local daemon if needed and opens the browser
/// on the wizard page.
pub async fn wizard_command(
    args: UiCommonArgs,
    no_telemetry: bool,
    watch: drain::Watch,
    user_data: &UserData,
) -> CliResult<()> {
    // The reporter fires a launch event on drop; `is-returning` is now tracked server-side by the
    // wizard's `cluster-details` endpoint once the user starts the config flow.
    let telemetry = !(no_telemetry || env::var("MIRRORD_TELEMETRY") == Ok("false".to_owned()));
    let _analytics = AnalyticsReporter::new(
        telemetry,
        ExecutionKind::Wizard,
        watch,
        user_data.machine_id(),
        None,
    );

    ui_command(args, Some(UiSubcommand::Start), "/wizard")
        .await
        .map_err(CliError::Ui)
}

/// The entrypoint for the `chaos` command. Starts the local daemon if needed without opening a
/// browser, then displays or edits active chaos rules.
pub async fn chaos_command(args: ChaosArgs) -> Result<(), UiCliError> {
    let details = ui_start(UI_DEFAULT_PORT, true, "").await?;
    info!(?details, "ran mirrord ui start");

    let sessions_dir = sessions_dir().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "failed to find home directory",
        )
    })?;

    let client = if let Some((_id, endpoint)) = session_endpoints(&sessions_dir)
        .iter()
        .find(|(id, _)| id == args.session_id())
    {
        SessionClient::new(endpoint.clone())
    } else {
        return Err(ChaosApiError::SessionNotFound(args.session_id().to_owned()))?;
    };

    let new_rules: Option<Vec<ChaosRuleRequest>> = if args.expects_rule() {
        let string_input = if let Some(file_path) = args.file_path() {
            std::fs::read_to_string(file_path)?
        } else {
            let mut buffer = String::new();
            let stdin = std::io::stdin();
            let mut handle = stdin.lock();
            handle.read_to_string(&mut buffer)?;
            buffer
        };

        // `VecOrSingle` can be created directly from `string_input` with `serde_json::from_str`,
        // but the error printed to the user is extremely unhelpful. To avoid this, deserialize the
        // rules one by one and stop on the first error.

        let outermost_value: Value = serde_json::from_str(&string_input)?;

        let rules: Vec<ChaosRuleRequest> = match outermost_value {
            Value::Array(values) => values
                .into_iter()
                .map(serde_json::from_value)
                .collect::<Result<Vec<ChaosRuleRequest>, _>>(),
            rule @ Value::Object(..) => serde_json::from_value(rule).map(|rule| vec![rule]),
            other => {
                return Err(ChaosApiError::BadRequest {
                    reason: format!(
                        "expected a chaos rule or list of rules in JSON format, found {}",
                        other
                    ),
                }
                .into());
            }
        }?;

        Some(rules)
    } else {
        None
    };

    let router_path = format!(
        "{BASE_INTPROXY_CHAOS_ROUTE}/{}",
        args.rule_id().unwrap_or("")
    );

    let requests = match &args.command {
        ChaosSubcommand::Add { .. } => new_rules
            .expect("args.expects_rule() requires rule(s) before match")
            .iter()
            .map(|new_rule| client.post(&router_path).json(&new_rule))
            .collect(),
        _ => {
            let request = match &args.command {
                ChaosSubcommand::List { .. } => client.get(router_path),
                ChaosSubcommand::Edit { .. } => {
                    let new_rule =
                        new_rules.expect("args.expects_rule() requires rule(s) before match");
                    if new_rule.len() == 1
                        && let Some(rule) = new_rule.first()
                    {
                        client.put(router_path).json(&rule)
                    } else {
                        return Err(ChaosApiError::BadRequest {
                            reason: format!(
                                "'chaos edit' command only accepts a single rule, found {} rules",
                                new_rule.len()
                            ),
                        }
                        .into());
                    }
                }
                ChaosSubcommand::Delete { .. } => client.delete(router_path),
                _ => unreachable!(),
            };
            vec![request]
        }
    };

    let responses: Vec<_> = requests
        .into_iter()
        .map(|request| async { request.send().await })
        .collect::<Vec<_>>();
    let responses: Vec<Response> = join_all(responses)
        .await
        .into_iter()
        .collect::<Result<Vec<Response>, SessionError>>()?;

    match (args.format, args.returns_json()) {
        (ChaosFormat::Pretty, returns_json) => {
            for response in responses.into_iter() {
                if response.status().is_success() {
                    if returns_json {
                        response
                            .json::<VecOrSingle<ChaosRule>>()
                            .await?
                            .iter()
                            .for_each(ChaosRule::pretty_print);
                    } else {
                        println!("Request sucess: status code {}", response.status())
                    }
                } else {
                    println!(
                        "Request failed: {}",
                        response
                            .bytes()
                            .await
                            .expect_err("response status is success")
                    )
                }
            }
        }
        (ChaosFormat::Json, true) => {
            let mut objects: Vec<Value> = vec![];
            for response in responses {
                let status = response.status();
                let value = match response.bytes().await {
                    Ok(bytes) => serde_json::from_slice(&bytes)?,
                    Err(error) => json!({
                        "error":
                            {
                                "status_code": status.as_u16(),
                                "body": error.to_string()
                            }
                    }),
                };
                objects.push(value);
            }
            match objects.as_array() {
                Some([single]) => {
                    println!("{single}")
                }
                _ => println!("{}", Value::from(objects)),
            }
        }
        (ChaosFormat::Json, false) | (ChaosFormat::Silent, _) => (),
    }

    Ok(())
}
