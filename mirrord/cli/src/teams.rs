use tokio::process::Command;

const MIRRORD_FOR_TEAMS_URL: &str = "https://mirrord.dev/docs/teams/introduction/";

#[cfg(not(target_os = "macos"))]
fn get_open_command() -> Command {
    let mut command = Command::new("gio");
    command.arg("open");
    command
}

#[cfg(target_os = "macos")]
fn get_open_command() -> Command {
    Command::new("open")
}

/// Attempts to open mirrord for Teams introduction in the default browser.
/// In case of failure, prints the link.
pub async fn navigate_to_intro() {
    match get_open_command().arg(MIRRORD_FOR_TEAMS_URL).output().await {
        Ok(output) if output.status.success() => {}
        other => {
            tracing::trace!("failed to open browser, command result: {other:?}");
            println!("mirrord failed to use a default browser");
            println!("To try mirrord for Teams, visit {MIRRORD_FOR_TEAMS_URL}");
        }
    }
}
