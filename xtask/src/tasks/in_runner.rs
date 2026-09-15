use std::{env, io::IsTerminal, path::Path, process::Command};

use anyhow::{Context, Result, bail};

/// Runs a command inside the CI runner image, which carries every toolchain the suites need and
/// stages the prebuilt test apps into the checkout on start.
pub fn run(image: String, command: Vec<String>) -> Result<()> {
    let mut docker = Command::new("docker");

    docker.args(["run", "--rm", "-i"]);

    if std::io::stdin().is_terminal() {
        docker.arg("-t");
    }

    if cfg!(target_os = "linux") {
        docker.args(["--network", "host"]);
    } else {
        docker.args(["--add-host", "host.docker.internal:host-gateway"]);
    }

    let root = env::current_dir().context("Failed to read the working directory")?;
    let home = dirs::home_dir().context("Failed to resolve the home directory")?;

    for mount in [
        mount(&root, "/workspace/mirrord", false),
        "ci-runner-cargo-registry:/usr/local/cargo/registry".to_owned(),
        "ci-runner-cargo-git:/usr/local/cargo/git".to_owned(),
        "ci-runner-go:/go".to_owned(),
        "ci-runner-target:/workspace/mirrord/target".to_owned(),
        mount(&home.join(".kube"), "/root/.kube", true),
    ] {
        docker.args(["-v", &mount]);
    }

    docker.args(["-e", "MIRRORD_TELEMETRY=false"]);
    docker.args(["-w", "/workspace/mirrord", &image]);
    docker.args(command);

    let status = docker.status().context("Failed to run Docker")?;

    if !status.success() {
        bail!("command failed inside {image}");
    }

    Ok(())
}

fn mount(host: &Path, container: &str, read_only: bool) -> String {
    let host = dunce::simplified(host).to_string_lossy().replace('\\', "/");
    let suffix = if read_only { ":ro" } else { "" };

    format!("{host}:{container}{suffix}")
}
