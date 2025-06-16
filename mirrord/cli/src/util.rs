use std::{io, io::Write, net::SocketAddr};

use mirrord_config::internal_proxy::MIRRORD_INTPROXY_CONTAINER_MODE_ENV;
use nix::libc;
use tokio::{net::TcpListener, process::Command};
use tracing::Level;

/// Address for mirrord-console is listening on.
pub(crate) const MIRRORD_CONSOLE_ADDR_ENV: &str = "MIRRORD_CONSOLE_ADDR";

/// User git branch (set by plugins).
pub(crate) const MIRRORD_BRANCH_NAME_ENV: &str = "MIRRORD_BRANCH_NAME";

/// Removes `HTTP_PROXY` and `https_proxy` from the environment
pub(crate) fn remove_proxy_env() {
    for (key, _val) in std::env::vars() {
        let lower_key = key.to_lowercase();
        if lower_key == "http_proxy" || lower_key == "https_proxy" {
            // we set instead of unset since this way extension
            // will be able to propogate it as well.
            std::env::set_var(key, "")
        }
    }
}

/// Used to pipe std[in/out/err] to "/dev/null" to prevent any printing to prevent any unwanted
/// side effects
unsafe fn redirect_fd_to_dev_null(fd: libc::c_int) {
    let devnull_fd = libc::open(b"/dev/null\0" as *const [u8; 10] as _, libc::O_RDWR);
    libc::dup2(devnull_fd, fd);
    libc::close(devnull_fd);
}

/// Create a new session for the proxy process, detaching from the original terminal.
/// This makes the process not to receive signals from the "mirrord" process or it's parent
/// terminal fixes some side effects such as <https://github.com/metalbear-co/mirrord/issues/1232>
pub(crate) unsafe fn detach_io() -> Result<(), nix::Error> {
    nix::unistd::setsid()?;

    // flush before redirection
    {
        // best effort
        let _ = std::io::stdout().lock().flush();
    }
    for fd in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
        redirect_fd_to_dev_null(fd);
    }
    Ok(())
}

/// Creates a listening socket using socket2
/// to control the backlog and manage scenarios where
/// the proxy is under heavy load.
/// <https://github.com/metalbear-co/mirrord/issues/1716#issuecomment-1663736500>
/// in macOS backlog is documented to be hardcoded limited to 128.
#[tracing::instrument(level = Level::TRACE, ret)]
pub(crate) fn create_listen_socket(addr: SocketAddr) -> io::Result<TcpListener> {
    let socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;

    socket.bind(&socket2::SockAddr::from(addr))?;
    socket.listen(1024)?;
    socket.set_nonblocking(true)?;

    // socket2 -> std -> tokio
    TcpListener::from_std(socket.into())
}

/// Returns whether this internal proxy process runs in the container mode.
///
/// Uses the [`MIRRORD_INTPROXY_CONTAINER_MODE_ENV`] variable.
pub fn intproxy_container_mode() -> bool {
    std::env::var(MIRRORD_INTPROXY_CONTAINER_MODE_ENV)
        .ok()
        .and_then(|value| value.parse::<bool>().ok())
        .unwrap_or_default()
}

/// Tries to retrieve the user's git branch from [`MIRRORD_BRANCH_NAME_ENV`] in env (set by the
/// plugins) and falls back to running 'git branch --show-current' to obtain current user git
/// branch, used for metrics reporting.
///
/// If any error is encountered, including if the user is not on a git branch, the command
/// produces an empty string, and the function returns `None`.
pub async fn get_user_git_branch() -> Option<String> {
    if let Ok(branch_name) = std::env::var(MIRRORD_BRANCH_NAME_ENV) {
        return Some(branch_name);
    }

    match Command::new("git")
        .args(["branch", "--show-current"])
        .output()
        .await
    {
        Ok(output) if output.status.success() => String::from_utf8(output.stdout)
            .ok()
            .map(|output| output.trim().to_string())
            .filter(|string| !string.is_empty()),
        Ok(output) => {
            tracing::debug!(
                status = %output.status,
                stderr = ?String::from_utf8_lossy(&output.stderr),
                "`git branch --show-current` command failed."
            );
            None
        }
        Err(error) => {
            tracing::debug!(
                %error,
                "Failed to execute `git branch` command, check that git is installed."
            );
            None
        }
    }
}
