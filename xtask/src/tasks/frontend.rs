use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};

use crate::relative_to_root;

pub fn package_dir(package_path: &str) -> PathBuf {
    relative_to_root(Path::new(package_path))
}

pub fn pnpm_install() -> Result<()> {
    let root_dir = relative_to_root(Path::new("."));

    if !pnpm_available() {
        anyhow::bail!(
            "`pnpm` not found on PATH. Install it before building frontend assets \
             (for example with `corepack enable pnpm`)."
        );
    }

    println!("  -> Running pnpm install...");
    let status = Command::new("pnpm")
        .arg("install")
        .current_dir(&root_dir)
        .status()
        .context("Failed to run pnpm install")?;
    if !status.success() {
        anyhow::bail!("pnpm install failed");
    }

    Ok(())
}

pub fn pnpm_build(package_name: &str) -> Result<()> {
    let root_dir = relative_to_root(Path::new("."));

    println!("  -> Running pnpm --filter {package_name} run build...");
    let status = Command::new("pnpm")
        .args(["--filter", package_name, "run", "build"])
        .current_dir(root_dir)
        .status()
        .with_context(|| format!("Failed to run pnpm build for {package_name}"))?;
    if !status.success() {
        anyhow::bail!("pnpm build failed for {package_name}");
    }

    Ok(())
}

fn pnpm_available() -> bool {
    Command::new("pnpm")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success())
}
