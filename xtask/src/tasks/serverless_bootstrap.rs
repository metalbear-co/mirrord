use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};

use crate::relative_to_root;

pub fn build_serverless_bootstrap(release: bool, cargo_args: &[String]) -> Result<PathBuf> {
    println!("Building mirrord serverless bootstrap...");

    let mode = if release { "release" } else { "debug" };
    let agent_binary = build_agent_binary(release, cargo_args)?;

    let mut cmd = Command::new("cargo");
    cmd.arg("build");
    cmd.arg("-p").arg("mirrord-serverless-bootstrap");

    if release {
        cmd.arg("--release");
    }

    cmd.env(
        "MIRRORD_AGENT_BINARY",
        agent_binary
            .canonicalize()
            .context("Failed to canonicalize mirrord-agent binary path")?,
    );
    cmd.args(cargo_args);

    let status = cmd.status().context("Failed to run cargo build")?;

    if !status.success() {
        anyhow::bail!("cargo build failed for mirrord-serverless-bootstrap");
    }

    let bootstrap_path = relative_to_root(
        Path::new("target")
            .join(mode)
            .join("libmirrord_serverless_bootstrap.so")
            .as_path(),
    );

    println!("✓ Serverless bootstrap built: {}", bootstrap_path.display());
    Ok(bootstrap_path)
}

fn build_agent_binary(release: bool, cargo_args: &[String]) -> Result<PathBuf> {
    println!("Building mirrord-agent for serverless bootstrap...");

    let mut cmd = Command::new("cargo");
    cmd.arg("build");
    cmd.arg("-p").arg("mirrord-agent");

    if release {
        cmd.arg("--release");
    }

    cmd.args(cargo_args);

    let status = cmd.status().context("Failed to run cargo build")?;

    if !status.success() {
        anyhow::bail!("cargo build failed for mirrord-agent");
    }

    let mode = if release { "release" } else { "debug" };
    let agent_binary = relative_to_root(
        Path::new("target")
            .join(mode)
            .join("mirrord-agent")
            .as_path(),
    );

    println!("✓ Agent built: {}", agent_binary.display());
    Ok(agent_binary)
}
