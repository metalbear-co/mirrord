//! # mirrord UI (Session Monitor)
//!
//! The `mirrord ui` command launches a web-based session monitor that aggregates events from
//! all active mirrord sessions. It watches `~/.mirrord/sessions/` for session sentinel files
//! (`.sock` on unix, `.pipe` on windows), connects to each session's HTTP API over its
//! transport (Unix domain socket or named pipe), and serves a React frontend plus
//! REST/SSE/WebSocket endpoints on localhost.

#[cfg(unix)]
use std::num::ParseIntError;
use std::{
    collections::HashMap,
    env::{temp_dir, vars},
    fs::File,
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    process::Stdio,
    str::FromStr,
    sync::Arc,
    time::{Duration, SystemTime},
};

use fs4::fs_std::FileExt;
use miette::Diagnostic;
use mirrord_progress::MIRRORD_PROGRESS_ENV;
use mirrord_session_monitor_client::sessions_dir;
#[cfg(unix)]
use nix::{
    errno::Errno,
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use rand::RngExt;
use thiserror::Error;
use tokio::{
    fs::create_dir_all,
    io::{AsyncBufReadExt, BufReader},
    sync::{Mutex, broadcast},
};
use tracing::{debug, error, warn};

use crate::{
    config::{UI_DEFAULT_PORT, UiArgs, UiSubcommand},
    ui::server::*,
    user_data::UserData,
    util::mirrord_dir::{self, get_path_and_create_with_fallback},
};

mod chaos;
mod error;
pub mod server;
mod wizard;

const MAX_EVENTS_PER_SESSION: usize = 500;

/// The name of the file that is locked by the currently running ui server. If a new server attempts
/// to start running, it will fail to lock this file and exit.
///
/// Only ever touched by the *child* process that runs the server, not the parent process running in
/// the foreground.
const UI_LOCK_FILE_NAME: &str = "ui.lock";

/// The name of the file where we store the PID of the most recently started ui server. We use this
/// to be able to kill the currently running server in a standalone command. If there is no lock on
/// [`UI_LOCK_FILE_NAME`], no server is running: this file is stale.
///
/// Only ever written, read or deleted by the parent (foreground) process, not the child process
/// that runs the ui server. Is not locked.
const PID_FILE_NAME: &str = "server_pid";

/// The name of the file containing the current auth token for the ui server.
///
/// Is written to by the child process (running the server) when it gets the file lock for
/// [`UI_LOCK_FILE_NAME`]. Is read by the child process for authentication, and the parent
/// (foreground) process when printing the ui server details to the user. Is not locked.
const TOKEN_FILE_NAME: &str = "token";

/// The header key that can be used to set the auth token in requests to the ui server. It can also
/// be set in a cookie (established by the `/auth` entry point). See the `ui::server::token_auth()`
/// middleware.
const TOKEN_HEADER_NAME: &str = "x-auth-token";

/// The name of the env var containing the port that the ui server should run on. If present in env,
/// indicated to the current invokation of `mirrord ui` that this process is a child process and
/// needs to run the server instead of performing setup and running in the foreground. If not
/// specified by the user, defaults to [`UI_DEFAULT_PORT`].
const MIRRORD_SERVER_PORT_ENV_NAME: &str = "MIRRORD_SPAWNED_SERVER_PORT";

// ===================================== cli code starts roughly here =============================

#[derive(Debug, Error, Diagnostic)]
pub enum UiCliError {
    #[error("the mirrord UI server process failed: {0}")]
    UiServer(#[from] UiServerError),

    /// IO error for the foreground process - for the server, use [`UiServerError::Io`].
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// May occur while trying to kill the existing UI server.
    #[cfg(unix)]
    #[error("failed to perform an operation on the UI server process: {0}")]
    #[diagnostic(help(
        "To forcefully stop the server process, try killing it manually. On \
        unix for example, find the process with `ps aux | grep mirrord ui` and \
        then `kill $PID` to stop it running."
    ))]
    Process(#[from] Errno),

    /// Occurs when the foreground task is waiting to read an "OK" message from the stdout of the
    /// child, and it times out or gets a different response or error.
    #[error("failed to communicate with the server background process: {0}")]
    SpawnBackgroundTask(String),

    /// May occur when the foreground task cannot get the child task's PID, indicating that the
    /// process has exited.
    #[error("the new server process ended unexpectedly")]
    ChildExitedUnexpectedly,

    #[cfg(unix)]
    #[error("failed to parse a PID from file contents: {0}")]
    PidParse(ParseIntError),
}

#[derive(Debug, Error, Diagnostic)]
pub enum UiServerError {
    /// IO error for the background process.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("failed to create watcher: {0}")]
    Watcher(#[from] notify::Error),
}

/// Keeps the UI lock file, `~/.mirrord/ui.lock`, open and exclusively locked for as long as the UI
/// server runs.
///
/// A running `mirrord ui` holds an exclusive lock on this file. The lock — released by the OS even
/// on a hard kill — is how a second invocation detects that one is already running. It is kept
/// separate from the token file so the token stays freely readable on all platforms.
///
/// Dropping the guard removes the token file, signalling that the UI is no longer running. A hard
/// kill skips this [`Drop`] and leaves the token file behind, but the OS releases the lock on
/// process exit, so the next invocation still sees the previous instance is gone, reclaims the
/// lock, and overwrites the stale token.
struct TokenFileGuard {
    /// Held open to keep the exclusive lock; the lock file itself is left in place (removing it
    /// while the handle is open is racy and a leftover empty file is harmless — the next run
    /// re-locks it). The lock releases when this handle closes, including on a hard kill.
    lock_file: File,

    /// The running `mirrord ui` writes its auth token, `~/.mirrord/token`, here so a second
    /// invocation can read it back and print a working URL. This file is never locked — on
    /// Windows an exclusive lock is mandatory and would stop the second process from reading
    /// it — so mutual exclusion lives in a separate lock file (`self.lock_file`).
    token_path: PathBuf,
}

impl Drop for TokenFileGuard {
    fn drop(&mut self) {
        if let Err(err) = std::fs::remove_file(&self.token_path) {
            println!(
                "Failed to remove UI token file at {}: {err}",
                self.token_path.display()
            );
        }
        let _ = FileExt::unlock(&self.lock_file);
    }
}

/// Result of trying to claim ownership of the UI lock.
enum TokenClaim {
    /// No other `mirrord ui` was running; we now hold the lock and published `token`.
    Claimed {
        guard: TokenFileGuard,
        token: String,
    },
    /// Another `mirrord ui` is already running; `token` is the one it published.
    AlreadyRunning,
}

impl TokenClaim {
    /// Tries to become the single running `mirrord ui` instance by taking an exclusive lock on the
    /// lock file. If another instance already holds it, reads back the token it published.
    pub fn claim_token_file() -> Result<TokenClaim, std::io::Error> {
        // ensure ~/.mirrord exists
        let mirrord_dir = get_path_and_create_with_fallback()?;

        Self::claim_token_file_at(
            mirrord_dir.join(UI_LOCK_FILE_NAME),
            mirrord_dir.join(TOKEN_FILE_NAME),
        )
    }

    fn claim_token_file_at(
        lock_path: PathBuf,
        token_path: PathBuf,
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
            },
            token,
        })
    }
}

/// Performs setup and starts the UI server, serving [`build_router()`]. Prints a message to
/// `stdout` when setup is completed successfully. Keeps ownership over [`UI_LOCK_FILE_NAME`] while
/// alive after writing the latest token value to [`TOKEN_FILE_NAME`] . If this instance cannot
/// claim the lock file, exits early with `Ok(())`.
///
/// Returns an error if setup fails, or when the server is running and exits due to an error.
async fn ui_run_server(port: u16) -> Result<(), UiServerError> {
    let (guard, token) = match TokenClaim::claim_token_file()? {
        TokenClaim::AlreadyRunning => {
            println!("SERVER: ui server already running");
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

    let state = AppState {
        sessions: Default::default(),
        operator_sessions: Default::default(),
        operator_watch_status: Default::default(),
        operator_license: Default::default(),
        notify_tx,
        token: token.clone(),
        user_data: Arc::new(Mutex::new(user_data)),
        clients: Default::default(),
    };

    scan_existing_sessions(&sessions_dir, &state).await;
    #[cfg(target_os = "macos")]
    start_periodic_rescan(sessions_dir.clone(), state.clone());
    start_filesystem_watcher(&sessions_dir, state.clone())?;
    start_operator_watcher(state.clone());

    let app = build_router(state);

    let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    // print OK to parent process so it can detach
    println!("SERVER: setup complete");
    debug!(?addr, ?token, "serving router for mirrord ui");

    // Held until the server stops so the token file is removed on graceful shutdown.
    let _guard = guard;
    axum::serve(listener, app)
        .await
        .map_err(UiServerError::from)
}

/// Starts the UI server. If [`MIRRORD_SERVER_PORT_ENV_NAME`] is present, runs [`ui_run_server()`]
/// as the background process. Otherwise, spawns the [`UiSubcommand::Start`] with
/// [`MIRRORD_SERVER_PORT_ENV_NAME`] set. The two processes perform setup tasks and the parent
/// (foreground) process ensures the cild (background) process started as expected, before printing
/// details to the user and exiting.
///
/// The server runs until it is killed by the user, either with [`UiSubcommand::Stop`] or `kill
/// $PID`.
///
/// `open_path` is the path the browser is pointed at (e.g. `/` for the session monitor, `/wizard`
/// for the config wizard), appended to the server URL before the `?token=` query.
pub async fn ui_start(port: u16, no_browser: bool, open_path: &str) -> Result<(), UiCliError> {
    let mirrord_binary = std::env::current_exe()?;

    let std_err_dir = temp_dir()
        .join("mirrord")
        .join(format!("ui-{}", env!("CARGO_PKG_VERSION")));
    create_dir_all(&std_err_dir).await?;
    let timestamp = SystemTime::UNIX_EPOCH
        .elapsed()
        .expect("system time should not be earlier than UNIX EPOCH")
        .as_secs();

    // stderr is piped into the file `/tmp/mirrord/ui-{MIRRORD_VERSION}/stderr-{timestamp}`
    // if a server is already running, logs will still get piped to this file
    let std_err_file = std_err_dir.join(format!("stderr-{timestamp}"));

    let mut env_vars: HashMap<String, String> = vars().collect();
    env_vars.insert(MIRRORD_SERVER_PORT_ENV_NAME.to_owned(), port.to_string());
    env_vars.insert(MIRRORD_PROGRESS_ENV.to_owned(), "off".to_owned());

    // default to debug level for logs sent to `std_err_file`
    if !env_vars.contains_key("RUST_LOG") {
        env_vars.insert("RUST_LOG".to_owned(), "mirrord=debug".to_owned());
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
    let server_already_running = match first_line {
        Err(..) => {
            return Err(UiCliError::SpawnBackgroundTask(
                "timed out waiting for the server process to confirm setup complete".to_owned(),
            ));
        }
        Ok(Err(error)) => {
            return Err(UiCliError::SpawnBackgroundTask(format!(
                "failed to read the server process' stdout with {error}",
            )));
        }
        Ok(Ok(None)) => {
            return Err(UiCliError::SpawnBackgroundTask(
                "unexpected EOF when reading the server process' stdout".to_owned(),
            ));
        }
        Ok(Ok(Some(line))) => {
            if line == "SERVER: setup complete" {
                false
            } else if line == "SERVER: ui server already running" {
                true
            } else {
                return Err(UiCliError::SpawnBackgroundTask(format!(
                    "unexpected message when reading the server process' stdout: {line}",
                )));
            }
        }
    };

    let pid_file = mirrord_dir::get_path_or_fallback().join(PID_FILE_NAME);
    let child_pid = if server_already_running {
        // read pid from file, and dont overwrite it
        std::fs::read_to_string(&pid_file).unwrap_or("unknown".to_owned())
    } else {
        // store the server (child) process ID so we can use it to kill the process when
        // `mirrord ui stop` is called.
        let Some(child_pid) = child.id().map(|pid| pid.to_string()) else {
            return Err(UiCliError::ChildExitedUnexpectedly);
        };

        if let Err(err) = std::fs::write(&pid_file, &child_pid) {
            // it's extremely unlikely to fail to write this file after we have the ui.lock
            // file, but notify the user anyway because it means the ui stop
            // command won't work as usual.
            println!(
                "Unable to save PID of the server process. This does not mean the server is not running, \
                but you may have to kill the process manually to stop it. Error: `{err}`"
            );
        }
        child_pid
    };

    let token_path = mirrord_dir::get_path_or_fallback().join(TOKEN_FILE_NAME);
    let token = std::fs::read_to_string(&token_path)?;
    let token = token.trim();

    // Open the `/auth` entry point (the only route that accepts the token in the query string); it
    // sets the cookie and redirects to `open_path` (`/` for the monitor, `/wizard` for the wizard).
    let url = format!(
        "http://{}:{port}/auth?token={token}&redirect={open_path}",
        Ipv4Addr::LOCALHOST
    );

    // open browser and print details to user
    if !(server_already_running || no_browser) {
        let _ = opener::open_browser(&url).map_err(|err| {
            warn!(?err, "Failed to open browser");
        });
    }
    ui_start_printout(
        server_already_running,
        &url,
        token,
        &child_pid,
        &std_err_file.to_string_lossy(),
    );

    Ok(())
}

fn ui_start_printout(
    already_running: bool,
    url: &str,
    token: &str,
    server_pid: &str,
    std_err_file: &str,
) {
    let mut lines = String::new();

    lines.push('\n');
    if already_running {
        lines.push_str("* Another session monitor is already running\n");
    } else {
        lines.push_str("* New mirrord session monitor started\n");
    }
    lines.push_str("* Server PID:\n");
    lines.push_str(format!(" -> {server_pid}\n").as_str());

    lines.push('\n');
    lines.push_str("* Web UI:\n");
    lines.push_str(format!(" -> {url}\n").as_str());
    lines.push_str("* API token:\n");
    lines.push_str(format!(" -> {TOKEN_HEADER_NAME}: {token}\n").as_str());

    lines.push('\n');
    if already_running {
        lines.push_str("* mirrord session monitor unchanged\n");
    } else {
        lines.push_str("* mirrord session monitor ready!\n");
        lines.push_str(format!(" -> server log file: {std_err_file}\n").as_str());
    }

    println!("{lines}")
}

/// Kills the UI server that is currently running by reading the contents of the file
/// [`PID_FILE_NAME`]. First checks a server is running by attempting to lock the file
/// [`UI_LOCK_FILE_NAME`]. Releases the lock after deleting stale files.
///
/// @with_printouts: if `true`, prints info messages to stdout. Does not affect logs.
pub async fn ui_stop(with_printouts: bool) -> Result<(), UiCliError> {
    let mirrord_dir = mirrord_dir::get_path_or_fallback();
    let pid_file = mirrord_dir.join(PID_FILE_NAME);
    let pid = std::fs::read_to_string(&pid_file)?;

    debug!(
        ?pid,
        ?pid_file,
        "UI server process ID read from file successfully"
    );

    let guard = match TokenClaim::claim_token_file()? {
        TokenClaim::AlreadyRunning => {
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
                println!("* Sent stop command to UI server (it may not exit immediately)");
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

    // remove stale files regardless of if the server was running already, ignore errors since they
    // won't cause problems being there
    let _ = std::fs::remove_file(mirrord_dir::get_path_or_fallback().join(PID_FILE_NAME))
        .inspect_err(|err| debug!(?err, "deleting PID file returned error"));
    let _ = std::fs::remove_file(mirrord_dir::get_path_or_fallback().join(TOKEN_FILE_NAME))
        .inspect_err(|err| debug!(?err, "deleting token file returned error"));

    if with_printouts {
        println!("* Cleaned up stale files");
    }

    drop(guard);
    Ok(())
}

/// Runs the `mirrord ui` command to either start or stop the UI server.
/// If starting a new server fails, runs `mirrord ui stop` silently to eliminate any leftover
/// process or files, ignoring errors.
///
/// `open_path` selects which page the browser opens on when the server starts (`/` for the session
/// monitor, `/wizard` for the config wizard). It has no effect on [`UiSubcommand::Stop`].
pub async fn ui_command(
    UiArgs {
        port,
        no_browser,
        command,
    }: UiArgs,
    open_path: &str,
) -> Result<(), UiCliError> {
    match command.unwrap_or(UiSubcommand::Start) {
        UiSubcommand::Start => {
            if let Ok(port) = std::env::var(MIRRORD_SERVER_PORT_ENV_NAME) {
                ui_run_server(u16::from_str(&port).unwrap_or(UI_DEFAULT_PORT)).await?;
                Ok(())
            } else {
                let res = ui_start(port, no_browser, open_path).await;
                if res.is_err() {
                    error!("`mirrord ui` failed to start the server, running `mirrord ui stop`");
                    let _ = ui_stop(false).await;
                }
                res
            }
        }
        UiSubcommand::Stop => ui_stop(true).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &tempfile::TempDir) -> (PathBuf, PathBuf) {
        (dir.path().join("ui.lock"), dir.path().join("token"))
    }

    /// The first claim takes the lock and writes a fresh token to the token file.
    #[test]
    fn first_claim_succeeds_and_writes_token() {
        let dir = tempfile::tempdir().unwrap();
        let (lock, token_path) = paths(&dir);

        let TokenClaim::Claimed { guard, token } =
            TokenClaim::claim_token_file_at(lock, token_path.clone()).unwrap()
        else {
            panic!("first claim should succeed");
        };

        assert!(!token.is_empty());
        assert_eq!(std::fs::read_to_string(&token_path).unwrap(), token);
        drop(guard);
    }

    /// While one claim holds the lock, a second claim reports the UI is already running and
    /// hands back the same token, so the caller can build a working URL.
    #[test]
    fn second_claim_while_held_returns_already_running_with_same_token() {
        let dir = tempfile::tempdir().unwrap();
        let (lock, token_path) = paths(&dir);

        let TokenClaim::Claimed { guard, token } =
            TokenClaim::claim_token_file_at(lock.clone(), token_path.clone()).unwrap()
        else {
            panic!("first claim should succeed");
        };

        match TokenClaim::claim_token_file_at(lock, token_path.clone()).unwrap() {
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
        let (lock, token_path) = paths(&dir);

        let TokenClaim::Claimed {
            guard,
            token: first,
        } = TokenClaim::claim_token_file_at(lock.clone(), token_path.clone()).unwrap()
        else {
            panic!("first claim should succeed");
        };
        drop(guard);
        assert!(
            !token_path.exists(),
            "guard drop should remove the token file"
        );

        let TokenClaim::Claimed { token: second, .. } =
            TokenClaim::claim_token_file_at(lock, token_path).unwrap()
        else {
            panic!("reclaim after drop should succeed");
        };
        assert_ne!(first, second, "a reclaim should publish a fresh token");
    }
}
