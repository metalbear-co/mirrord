use tokio::process::Command;


const COMPRESSED_FRONTEND: &[u8] = include_bytes!("../../../wizard-frontend.tar.gz");
#[cfg(target_os = "linux")]
fn get_open_command() -> Command {
    let mut command = Command::new("gio");
    command.arg("open");
    command
}

#[cfg(target_os = "macos")]
fn get_open_command() -> Command {
    Command::new("open")
}

/// WARNING: untested on `target_os = "windows"`, but if this fails the URL will get printed to the
/// terminal as a fallback.
#[cfg(target_os = "windows")]
fn get_open_command() -> Command {
    Command::new("cmd.exe").arg("/C").arg("start").arg("")
}

pub async fn wizard_command() {
    // TODO: serve wizard <|:)

    // open browser window
    let url = "localhost:3000";
    match crate::wizard::get_open_command().arg(&url).output().await {
        Ok(output) if output.status.success() => {}
        other => {
            tracing::trace!(?other, "failed to open browser");
            println!(
                "To open the mirrord wizard, visit:\n\n\
             {url}"
            );
        }
    }
}
