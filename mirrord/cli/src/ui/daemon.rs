//! Local daemon discovery, authenticated control, and internal HTTP routes.
//!
//! The daemon and browser UI server are the same persistent process. Foreground mirrord
//! invocations discover that process through `daemon.json` and authenticate with the token file.
//! Shutdown is requested over the internal API because the daemon must atomically check its own DB
//! port-forward claims before deciding whether it can exit.

use std::{
    collections::HashMap,
    env::{self, temp_dir, vars},
    fs::File,
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::{Duration, SystemTime},
};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use fs4::fs_std::FileExt;
use mirrord_progress::MIRRORD_PROGRESS_ENV;
use mirrord_session_monitor_client::sessions_dir;
#[cfg(unix)]
use nix::{
    errno::Errno,
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use tokio::{
    fs::create_dir_all,
    io::{AsyncBufReadExt, BufReader},
    sync::{Mutex, broadcast},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

#[cfg(target_os = "macos")]
use super::server::start_periodic_rescan;
use super::{
    UiCliError, UiServerError, db_portforwards,
    server::{
        AppState, SessionNotification, build_router, scan_existing_sessions,
        start_filesystem_watcher, start_operator_watcher, token_auth,
    },
};
use crate::{
    config::UI_DEFAULT_PORT,
    data::UserData,
    util::mirrord_dir::{self, get_path_and_create_with_fallback},
};

/// The file locked by the running local daemon. A second daemon cannot acquire it and exits.
///
/// Only ever touched by the child process that runs the daemon, not the parent process running in
/// the foreground.
const UI_LOCK_FILE_NAME: &str = "ui.lock";

/// The file containing the PID of the most recently started daemon.
///
/// It allows failed startup to stop the background process. If [`UI_LOCK_FILE_NAME`] is not locked,
/// no daemon is running and this file is stale.
///
/// Only ever written, read or deleted by the parent (foreground) process, not the child process
/// that runs the daemon. It is not locked.
const PID_FILE_NAME: &str = "server_pid";

/// The file containing the current daemon HTTP authentication token.
///
/// The daemon writes it after acquiring [`UI_LOCK_FILE_NAME`]. Local mirrord processes read it to
/// authenticate internal API requests, while `mirrord ui` also uses it to establish browser access.
/// It is deliberately not locked so other local processes can read it.
const TOKEN_FILE_NAME: &str = "token";

/// Published by the daemon after binding its listener so local mirrord processes can discover the
/// actual port, including when the UI was started with a non-default port.
const DAEMON_INFO_FILE_NAME: &str = "daemon.json";
/// Version of the internal daemon discovery and HTTP protocol understood by this CLI.
const DAEMON_PROTOCOL_VERSION: u32 = 1;

/// Header used to authenticate daemon requests. Browser access can instead use the cookie
/// established by `/auth`; see the `ui::server::token_auth()` middleware.
pub(super) const TOKEN_HEADER_NAME: &str = "x-auth-token";

/// Port passed to the child mirrord process that becomes the local daemon.
///
/// Its presence tells `mirrord ui` to run the long-lived HTTP server instead of the foreground
/// startup path. If the user does not specify a port, startup uses [`UI_DEFAULT_PORT`].
pub(super) const MIRRORD_SERVER_PORT_ENV_NAME: &str = "MIRRORD_SPAWNED_SERVER_PORT";

/// Keeps `~/.mirrord/ui.lock` exclusively locked for the lifetime of the local daemon.
///
/// The lock, released by the OS even on a hard kill, is how another invocation detects that a
/// daemon is already running. It is separate from the token file so the token stays freely readable
/// on all platforms.
///
/// Dropping the guard removes daemon discovery and authentication files. A hard kill skips this
/// [`Drop`] and leaves those files behind, but the OS releases the lock on process exit. The next
/// invocation therefore sees that the previous instance is gone, reclaims the lock, and overwrites
/// the stale files.
pub(super) struct TokenFileGuard {
    /// Held open to keep the exclusive lock; the lock file itself is left in place (removing it
    /// while the handle is open is racy and a leftover empty file is harmless — the next run
    /// re-locks it). The lock releases when this handle closes, including on a hard kill.
    lock_file: File,

    /// The daemon writes its auth token, `~/.mirrord/token`, here so local mirrord processes can
    /// authenticate requests and `mirrord ui` can print a working URL. This file is never locked —
    /// on Windows an exclusive lock is mandatory and would stop the second process from
    /// reading it — so mutual exclusion lives in a separate lock file (`self.lock_file`).
    token_path: PathBuf,
    daemon_info_path: PathBuf,
}

impl Drop for TokenFileGuard {
    fn drop(&mut self) {
        if let Err(err) = std::fs::remove_file(&self.token_path) {
            println!(
                "Failed to remove UI token file at {}: {err}",
                self.token_path.display()
            );
        }
        let _ = std::fs::remove_file(&self.daemon_info_path);
        let _ = FileExt::unlock(&self.lock_file);
    }
}

/// Result of trying to claim ownership of the local daemon lock.
pub(super) enum TokenClaim {
    /// No daemon was running; this process now holds the lock and published `token`.
    Claimed {
        guard: TokenFileGuard,
        token: String,
    },
    /// Another daemon already holds the lock.
    AlreadyRunning,
}

impl TokenClaim {
    /// Tries to become the single local daemon by taking an exclusive lock on the lock file.
    /// Returns [`TokenClaim::AlreadyRunning`] when another process already holds it.
    pub fn claim_token_file() -> Result<TokenClaim, std::io::Error> {
        // ensure ~/.mirrord exists
        let mirrord_dir = get_path_and_create_with_fallback()?;

        Self::claim_token_file_at(
            mirrord_dir.join(UI_LOCK_FILE_NAME),
            mirrord_dir.join(TOKEN_FILE_NAME),
            mirrord_dir.join(DAEMON_INFO_FILE_NAME),
        )
    }

    pub(super) fn claim_token_file_at(
        lock_path: PathBuf,
        token_path: PathBuf,
        daemon_info_path: PathBuf,
    ) -> Result<TokenClaim, std::io::Error> {
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)?;

        if !lock_file.try_lock_exclusive()? {
            return Ok(TokenClaim::AlreadyRunning);
        }

        let token_bytes: [u8; 32] = rand::rng().random();
        let token = hex::encode(token_bytes);
        std::fs::write(&token_path, &token)?;

        Ok(TokenClaim::Claimed {
            guard: TokenFileGuard {
                lock_file,
                token_path,
                daemon_info_path,
            },
            token,
        })
    }
}

/// Initializes and runs the local daemon, serving [`build_router()`].
///
/// The daemon owns session discovery, internal APIs, browser UI routes, and shared DB forwards. It
/// publishes its authentication token and bound address, then prints a setup message so the
/// foreground parent can detach. It holds [`UI_LOCK_FILE_NAME`] for its lifetime; if another daemon
/// already holds the lock, this process exits early with `Ok(())`.
///
/// Returns an error if setup fails, or when the running daemon exits due to an error.
pub(super) async fn ui_run_server(port: u16) -> Result<(), UiServerError> {
    let (guard, token) = match TokenClaim::claim_token_file()? {
        TokenClaim::AlreadyRunning => {
            println!("SERVER: daemon already running");
            return Ok(());
        }
        TokenClaim::Claimed { guard, token } => (guard, token),
    };

    let sessions_dir = sessions_dir().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "failed to find home directory",
        )
    })?;

    std::fs::create_dir_all(&sessions_dir)?;

    let (notify_tx, _) = broadcast::channel::<SessionNotification>(256);

    let user_data = UserData::from_default_path().await.unwrap_or_default();
    let shutdown = CancellationToken::new();

    let state = AppState {
        sessions: Default::default(),
        operator_sessions: Default::default(),
        operator_watch_status: Default::default(),
        operator_license: Default::default(),
        notify_tx,
        token: token.clone(),
        user_data: Arc::new(Mutex::new(user_data)),
        clients: Default::default(),
        db_portforwards: Default::default(),
        shutdown: shutdown.clone(),
    };

    scan_existing_sessions(&sessions_dir, &state).await;
    #[cfg(target_os = "macos")]
    start_periodic_rescan(sessions_dir.clone(), state.clone());
    start_filesystem_watcher(&sessions_dir, state.clone())?;
    start_operator_watcher(state.clone());

    let app = build_router(state);

    let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let addr = listener.local_addr()?;
    std::fs::write(
        &guard.daemon_info_path,
        serde_json::to_vec(&DaemonInfo {
            protocol_version: DAEMON_PROTOCOL_VERSION,
            addr,
        })
        .expect("daemon discovery data serializes"),
    )?;

    // print OK to parent process so it can detach
    println!("SERVER: setup complete");
    debug!(?addr, ?token, "serving router for mirrord ui");

    // Held until the daemon stops so discovery files are removed on graceful shutdown.
    let _guard = guard;
    let graceful_shutdown = shutdown.clone();
    let server = async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(graceful_shutdown.cancelled_owned())
            .await
    };
    tokio::pin!(server);

    tokio::select! {
        result = &mut server => result.map_err(UiServerError::from),
        _ = shutdown.cancelled() => {
            match tokio::time::timeout(Duration::from_secs(2), &mut server).await {
                Ok(result) => result.map_err(UiServerError::from),
                Err(_) => {
                    warn!("forcing local daemon shutdown with connections still open");
                    Ok(())
                }
            }
        }
    }
}

/// Starts or reuses the local daemon and optionally opens its browser-facing UI.
///
/// The foreground process spawns another mirrord executable with
/// [`MIRRORD_SERVER_PORT_ENV_NAME`] set. That child runs [`ui_run_server`] and outlives the command
/// or mirrord session that started it. The parent waits for startup confirmation before returning.
///
/// The daemon remains running after the foreground command exits and is shared by later sessions.
///
/// `open_path` is the path the browser is pointed at (e.g. `/` for the session monitor, `/wizard`
/// for the config wizard), appended to the daemon URL before the `?token=` query.
pub(super) async fn ui_start(
    port: u16,
    no_browser: bool,
    open_path: &str,
) -> Result<ServerDetails, UiCliError> {
    let mirrord_binary = env::current_exe()?;

    let std_err_dir = temp_dir()
        .join("mirrord")
        .join(format!("ui-{}", env!("CARGO_PKG_VERSION")));
    create_dir_all(&std_err_dir).await?;
    let timestamp = SystemTime::UNIX_EPOCH
        .elapsed()
        .expect("system time should not be earlier than UNIX EPOCH")
        .as_secs();

    // stderr is piped into `/tmp/mirrord/ui-{MIRRORD_VERSION}/stderr-{timestamp}`. If a daemon is
    // already running, this short-lived child still writes its startup logs to the new file.
    let std_err_file = std_err_dir.join(format!("stderr-{timestamp}"));

    let mut env_vars: HashMap<String, String> = vars().collect();
    env_vars.insert(MIRRORD_SERVER_PORT_ENV_NAME.to_owned(), port.to_string());
    env_vars.insert(MIRRORD_PROGRESS_ENV.to_owned(), "off".to_owned());

    // default to debug level for logs sent to `std_err_file`
    if !env_vars.contains_key("MIRRORD_LOG") {
        env_vars.insert("MIRRORD_LOG".to_owned(), "mirrord=debug".to_owned());
    }

    let mut child = tokio::process::Command::new(mirrord_binary)
        .args(vec!["ui"])
        .envs(env_vars)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(File::create(&std_err_file)?)
        .kill_on_drop(false)
        .spawn()?;

    let mut stdout = BufReader::new(child.stdout.take().expect("was piped")).lines();

    let first_line = tokio::time::timeout(Duration::from_secs(30), stdout.next_line()).await;
    let already_running = match first_line {
        Err(..) => {
            return Err(UiCliError::SpawnBackgroundTask(
                "timed out waiting for the daemon process to confirm setup complete".to_owned(),
            ));
        }
        Ok(Err(error)) => {
            return Err(UiCliError::SpawnBackgroundTask(format!(
                "failed to read the daemon process' stdout with {error}",
            )));
        }
        Ok(Ok(None)) => {
            return Err(UiCliError::SpawnBackgroundTask(
                "unexpected EOF when reading the daemon process' stdout".to_owned(),
            ));
        }
        Ok(Ok(Some(line))) => {
            if line == "SERVER: setup complete" {
                false
            } else if line == "SERVER: daemon already running" {
                true
            } else {
                return Err(UiCliError::SpawnBackgroundTask(format!(
                    "unexpected message when reading the daemon process' stdout: {line}",
                )));
            }
        }
    };

    let pid_file = mirrord_dir::get_path_or_fallback().join(PID_FILE_NAME);
    let server_pid = if already_running {
        // read pid from file, and dont overwrite it
        std::fs::read_to_string(&pid_file).unwrap_or("unknown".to_owned())
    } else {
        // Store the daemon process ID for startup cleanup and diagnostics.
        let Some(child_pid) = child.id().map(|pid| pid.to_string()) else {
            return Err(UiCliError::ChildExitedUnexpectedly);
        };

        if let Err(err) = std::fs::write(&pid_file, &child_pid) {
            // it's extremely unlikely to fail to write this file after we have the ui.lock
            // file, but notify the user anyway because manual daemon cleanup will be harder.
            println!(
                "Unable to save PID of the daemon process. This does not mean the daemon is not running, \
                but you may have to kill the process manually to stop it. Error: `{err}`"
            );
        }
        child_pid
    };

    let token_path = mirrord_dir::get_path_or_fallback().join(TOKEN_FILE_NAME);
    let fallback_token = std::fs::read_to_string(&token_path)?;
    let fallback_token = fallback_token.trim().to_owned();
    let info = read_daemon_info().unwrap_or(DaemonInfo {
        protocol_version: DAEMON_PROTOCOL_VERSION,
        addr: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port),
    });

    // Open the `/auth` entry point (the only route that accepts the token in the query string); it
    // sets the cookie and redirects to `open_path` (`/` for the monitor, `/wizard` for the wizard).
    let url = format!(
        "http://{}/auth?token={}&redirect={open_path}",
        info.addr, fallback_token
    );

    // open browser and print details to user
    if !no_browser {
        let _ = opener::open_browser(&url).map_err(|err| {
            warn!(?err, "Failed to open browser");
        });
    }

    Ok(ServerDetails {
        already_running,
        url,
        token: fallback_token,
        server_pid,
        std_err_file,
    })
}

/// Details of the daemon process that was started or already running.
#[derive(Debug)]
pub(super) struct ServerDetails {
    pub(super) already_running: bool,
    pub(super) url: String,
    pub(super) token: String,
    pub(super) server_pid: String,
    pub(super) std_err_file: PathBuf,
}

/// Discovery data published in `daemon.json` after the daemon binds its loopback listener.
///
/// The protocol version prevents a CLI from sending internal requests to an incompatible daemon;
/// `addr` supports ephemeral and user-selected ports rather than assuming [`UI_DEFAULT_PORT`].
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct DaemonInfo {
    pub(super) protocol_version: u32,
    pub(super) addr: SocketAddr,
}

/// Reads compatible daemon discovery data, rejecting missing, malformed, or differently versioned
/// files as unavailable.
pub(super) fn read_daemon_info() -> Option<DaemonInfo> {
    let path = mirrord_dir::get_path_or_fallback().join(DAEMON_INFO_FILE_NAME);
    let info: DaemonInfo = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (info.protocol_version == DAEMON_PROTOCOL_VERSION).then_some(info)
}

/// Authenticated client for internal APIs exposed by the local daemon.
#[derive(Clone)]
pub(crate) struct DaemonClient {
    client: reqwest::Client,
    info: DaemonInfo,
    token: String,
}

impl DaemonClient {
    fn discover() -> Result<Option<Self>, UiCliError> {
        let Some(info) = read_daemon_info() else {
            return Ok(None);
        };
        let token = match std::fs::read_to_string(
            mirrord_dir::get_path_or_fallback().join(TOKEN_FILE_NAME),
        ) {
            Ok(token) => token,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        Ok(Some(Self {
            client: reqwest::Client::new(),
            info,
            token: token.trim().to_owned(),
        }))
    }

    async fn ping(&self) -> bool {
        self.client
            .get(format!("http://{}/api/internal/ping", self.info.addr))
            .header(TOKEN_HEADER_NAME, &self.token)
            .timeout(Duration::from_secs(1))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
    }

    /// Requests attachment to a daemon-owned DB forward, retrying the session-discovery race.
    ///
    /// The intproxy publishes its session-monitor sentinel before making this request, but the
    /// daemon's filesystem watcher may not have registered it yet. The attach endpoint reports that
    /// state as `409 Conflict`; retrying for up to ten seconds lets discovery catch up without
    /// accepting claims from unknown sessions. Connection failures are retried for the same window
    /// because the daemon may still be completing startup.
    pub(crate) async fn attach_db_portforward(
        &self,
        request: &db_portforwards::DbPortForwardAttachRequest,
    ) -> Result<db_portforwards::DbPortForwardAttachResponse, UiCliError> {
        let url = format!(
            "http://{}/api/internal/db-port-forwards/attach",
            self.info.addr
        );
        for _ in 0..200 {
            let response = match self
                .client
                .post(&url)
                .header(TOKEN_HEADER_NAME, &self.token)
                .json(request)
                .send()
                .await
            {
                Ok(response) => response,
                Err(error) if error.is_connect() => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            if response.status() == reqwest::StatusCode::CONFLICT {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(UiCliError::DaemonResponse(format!("{status}: {body}")));
            }
            return response.json().await.map_err(UiCliError::from);
        }
        Err(UiCliError::DaemonResponse(
            "daemon did not accept the DB port-forward attachment within 10 seconds".to_owned(),
        ))
    }

    /// Asks the daemon to stop. The daemon performs the claim check and shutdown transition under
    /// the same registry lock used by new DB-forward attachments.
    async fn request_shutdown(&self) -> Result<(), UiCliError> {
        let response = self
            .client
            .post(format!("http://{}/api/internal/shutdown", self.info.addr))
            .header(TOKEN_HEADER_NAME, &self.token)
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::CONFLICT {
            let blocked: DaemonShutdownBlocked = response.json().await?;
            return Err(UiCliError::DaemonInUse(blocked.sessions.join(", ")));
        }
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(UiCliError::DaemonResponse(format!(
                "failed to stop the local daemon: {status}: {body}"
            )));
        }
        Ok(())
    }

    /// Waits for the daemon's discovery guard to be dropped after shutdown was accepted.
    ///
    /// The shutdown response is sent before Axum finishes draining connections. `daemon.json`
    /// disappears when the server future returns and the guard is dropped, which also removes the
    /// token and unlocks `ui.lock`. Waiting prevents an immediately started session from
    /// rediscovering a daemon that has accepted shutdown but has not finished exiting.
    async fn wait_for_exit(&self) -> Result<(), UiCliError> {
        for _ in 0..200 {
            if read_daemon_info().is_none() {
                let _ =
                    std::fs::remove_file(mirrord_dir::get_path_or_fallback().join(PID_FILE_NAME));
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        Err(UiCliError::DaemonResponse(
            "timed out waiting for the local daemon to stop".to_owned(),
        ))
    }
}

/// Discovers or starts the local daemon and returns an authenticated internal client.
///
/// `daemon.json` supplies the loopback address and compatible protocol version, while the token
/// file supplies authentication. Existing discovery data is trusted only after an authenticated
/// ping succeeds. Otherwise this starts a daemon without opening a browser, then reads the newly
/// published discovery information. Incompatible protocol versions are never reused.
pub(crate) async fn ensure_daemon() -> Result<DaemonClient, UiCliError> {
    if let Some(daemon) = DaemonClient::discover()?
        && daemon.ping().await
    {
        return Ok(daemon);
    }

    let details = ui_start(UI_DEFAULT_PORT, true, "").await?;
    let info = read_daemon_info().ok_or_else(|| {
        UiCliError::DaemonResponse(
            "an incompatible local mirrord daemon is already running; stop that process before retrying"
                .to_owned(),
        )
    })?;
    Ok(DaemonClient {
        client: reqwest::Client::new(),
        info,
        token: details.token,
    })
}

/// Stops the local daemon during failed startup or explicit internal cleanup.
///
/// If [`UI_LOCK_FILE_NAME`] is held, this kills the daemon using [`PID_FILE_NAME`]. Otherwise it
/// temporarily claims the lock and removes stale discovery, token, and PID files. Normal
/// `mirrord ui stop` uses the authenticated shutdown endpoint instead; this is only recovery for a
/// failed startup path.
///
/// @with_printouts: if `true`, prints info messages to stdout. Does not affect logs.
pub(super) async fn stop_daemon(with_printouts: bool) -> Result<(), UiCliError> {
    let mirrord_dir = mirrord_dir::get_path_or_fallback();
    let pid_file = mirrord_dir.join(PID_FILE_NAME);

    let guard = match TokenClaim::claim_token_file()? {
        TokenClaim::AlreadyRunning => {
            let pid = match std::fs::read_to_string(&pid_file) {
                Ok(pid) => pid,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Err(UiCliError::MissingPidFile);
                }
                Err(error) => return Err(error.into()),
            };

            debug!(
                ?pid,
                ?pid_file,
                "local daemon process ID read from file successfully"
            );

            #[cfg(unix)]
            {
                let pid = pid.parse().map_err(UiCliError::PidParse)?;
                kill(Pid::from_raw(pid), Some(Signal::SIGKILL)).or_else(|error| {
                    if error == Errno::ESRCH {
                        // ESRCH means that the process has already exited.
                        Ok(())
                    } else {
                        Err(error)
                    }
                })?;
            }

            #[cfg(windows)]
            std::process::Command::new("taskkill")
                .args(["/pid", &pid, "/t"])
                .output()?;

            if with_printouts {
                println!("* Sent stop command to local daemon (it may not exit immediately)");
            }
            None
        }
        TokenClaim::Claimed { guard, .. } => {
            if with_printouts {
                println!(
                    "* No running instance of `mirrord ui` was found. If you think this is incorrect, \
                try killing the process manually, for example by running `ps aux | grep mirrord` \
                and then `kill $PID` in a terminal."
                );
            }
            Some(guard)
        }
    };

    // Remove stale files, ignoring errors since they won't cause problems being there. When we
    // acquired the guard, it owns cleanup of the token file.
    let _ = std::fs::remove_file(mirrord_dir::get_path_or_fallback().join(PID_FILE_NAME))
        .inspect_err(|err| debug!(?err, "deleting PID file returned error"));
    let _ = std::fs::remove_file(mirrord_dir::get_path_or_fallback().join(DAEMON_INFO_FILE_NAME))
        .inspect_err(|err| debug!(?err, "deleting daemon info file returned error"));
    if guard.is_none() {
        let _ = std::fs::remove_file(mirrord_dir::get_path_or_fallback().join(TOKEN_FILE_NAME))
            .inspect_err(|err| debug!(?err, "deleting token file returned error"));
    }

    if with_printouts {
        println!("* Cleaned up stale files");
    }

    drop(guard);
    Ok(())
}

/// Stops the daemon unless active sessions still claim daemon-owned DB port forwards.
///
/// This foreground CLI process cannot safely check claims and then kill the daemon: a session could
/// attach between those operations. Instead, the daemon atomically checks and begins shutdown, and
/// this client waits for its guard-driven cleanup to finish.
pub async fn ui_stop(with_printouts: bool) -> Result<(), UiCliError> {
    let Some(daemon) = DaemonClient::discover()? else {
        if with_printouts {
            println!("* No running local mirrord daemon was found");
        }
        return Ok(());
    };

    daemon.request_shutdown().await?;
    daemon.wait_for_exit().await?;

    if with_printouts {
        println!("* Local mirrord daemon stopped");
    }
    Ok(())
}

async fn daemon_ping() -> StatusCode {
    StatusCode::OK
}

#[derive(Debug, Deserialize, Serialize)]
struct DaemonShutdownBlocked {
    sessions: Vec<String>,
}

async fn daemon_shutdown(State(state): State<AppState>) -> Response {
    match db_portforwards::request_daemon_shutdown(&state.db_portforwards, &state.shutdown).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(sessions) => (
            StatusCode::CONFLICT,
            Json(DaemonShutdownBlocked { sessions }),
        )
            .into_response(),
    }
}

/// Internal authenticated routes used by local mirrord processes to control daemon-owned services.
pub(super) fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/ping", get(daemon_ping))
        .route("/db-port-forwards/attach", post(db_portforwards::attach))
        .route("/shutdown", post(daemon_shutdown))
        .layer(middleware::from_fn_with_state(state, token_auth))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &tempfile::TempDir) -> (PathBuf, PathBuf, PathBuf) {
        (
            dir.path().join("ui.lock"),
            dir.path().join("token"),
            dir.path().join("daemon.json"),
        )
    }

    /// The first claim takes the lock and writes a fresh token to the token file.
    #[test]
    fn first_claim_succeeds_and_writes_token() {
        let dir = tempfile::tempdir().unwrap();
        let (lock, token_path, daemon_info_path) = paths(&dir);

        let TokenClaim::Claimed { guard, token } =
            TokenClaim::claim_token_file_at(lock, token_path.clone(), daemon_info_path).unwrap()
        else {
            panic!("first claim should succeed");
        };

        assert!(!token.is_empty());
        assert_eq!(std::fs::read_to_string(&token_path).unwrap(), token);
        drop(guard);
    }

    /// While one claim holds the lock, a second claim reports the daemon is already running. The
    /// caller reads the published token separately to build an authenticated client or UI URL.
    #[test]
    fn second_claim_while_held_returns_already_running_with_same_token() {
        let dir = tempfile::tempdir().unwrap();
        let (lock, token_path, daemon_info_path) = paths(&dir);

        let TokenClaim::Claimed { guard, token } = TokenClaim::claim_token_file_at(
            lock.clone(),
            token_path.clone(),
            daemon_info_path.clone(),
        )
        .unwrap() else {
            panic!("first claim should succeed");
        };

        match TokenClaim::claim_token_file_at(lock, token_path.clone(), daemon_info_path).unwrap() {
            TokenClaim::AlreadyRunning => (),
            TokenClaim::Claimed { .. } => panic!("second claim should see the lock held"),
        }

        let seen = std::fs::read_to_string(&token_path).expect("failed to read token file");
        assert_eq!(token, seen.trim());

        drop(guard);
    }

    /// Dropping the guard removes the token file and releases the lock, so a later invocation
    /// claims it fresh — this is how a clean shutdown signals the UI is gone.
    #[test]
    fn dropping_guard_removes_file_and_allows_reclaim() {
        let dir = tempfile::tempdir().unwrap();
        let (lock, token_path, daemon_info_path) = paths(&dir);

        let TokenClaim::Claimed {
            guard,
            token: first,
        } = TokenClaim::claim_token_file_at(
            lock.clone(),
            token_path.clone(),
            daemon_info_path.clone(),
        )
        .unwrap()
        else {
            panic!("first claim should succeed");
        };
        drop(guard);
        assert!(
            !token_path.exists(),
            "guard drop should remove the token file"
        );

        let TokenClaim::Claimed { token: second, .. } =
            TokenClaim::claim_token_file_at(lock, token_path, daemon_info_path).unwrap()
        else {
            panic!("reclaim after drop should succeed");
        };
        assert_ne!(first, second, "a reclaim should publish a fresh token");
    }
}
