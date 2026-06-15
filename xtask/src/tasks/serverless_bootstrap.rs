use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};

use super::{layer::Target, release::Platform};
use crate::relative_to_root;

pub fn build_serverless_bootstrap(
    platform: Platform,
    release: bool,
    cargo_args: &[String],
) -> Result<PathBuf> {
    println!("Building mirrord serverless bootstrap...");

    let target = match platform {
        Platform::LinuxX86_64 => Target::LinuxX86_64,
        Platform::LinuxAarch64 => Target::LinuxAarch64,
        _ => anyhow::bail!("serverless bootstrap can only be built for Linux targets"),
    };

    let mode = if release { "release" } else { "debug" };
    let agent_binary = build_agent_binary(target, release, cargo_args)?;

    let mut cmd = Command::new("cargo");
    cmd.arg("build");
    cmd.arg("-p").arg("mirrord-serverless-bootstrap");

    if release {
        cmd.arg("--release");
    }

    let target_triple = target.triple();
    cmd.arg("--target").arg(&target_triple);
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
            .join(target_triple)
            .join(mode)
            .join("libmirrord_serverless_bootstrap.so")
            .as_path(),
    );

    println!("✓ Serverless bootstrap built: {}", bootstrap_path.display());
    Ok(bootstrap_path)
}

fn build_agent_binary(target: Target, release: bool, cargo_args: &[String]) -> Result<PathBuf> {
    println!("Building mirrord-agent for serverless bootstrap...");

    let mut cmd = Command::new("cargo");
    cmd.arg("build");
    cmd.arg("-p").arg("mirrord-agent");

    if release {
        cmd.arg("--release");
    }

    let target_triple = target.triple();
    cmd.arg("--target").arg(&target_triple);
    cmd.args(cargo_args);

    let status = cmd.status().context("Failed to run cargo build")?;

    if !status.success() {
        anyhow::bail!("cargo build failed for mirrord-agent");
    }

    let mode = if release { "release" } else { "debug" };
    let agent_binary = relative_to_root(
        Path::new("target")
            .join(target_triple)
            .join(mode)
            .join("mirrord-agent")
            .as_path(),
    );

    println!("✓ Agent built: {}", agent_binary.display());
    Ok(agent_binary)
}
