use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};

/// Builds the monitor frontend (pnpm install + build).
///
/// Fails loudly if `pnpm` is not on PATH so we don't silently ship a binary with an
/// empty `packages/monitor/dist` (which would 404 every UI request). On CI use
/// `corepack enable` to activate the pnpm shim from the `packageManager` pin in the
/// root `package.json`.
pub fn build_monitor() -> Result<PathBuf> {
    println!("Building monitor frontend...");

    let monitor_dir = Path::new("packages/monitor");

    if !monitor_dir.exists() {
        anyhow::bail!(
            "Monitor directory not found at {}. Are you in the mirrord repository root?",
            monitor_dir.display()
        );
    }

    if !pnpm_available() {
        anyhow::bail!(
            "`pnpm` not found on PATH — required to build the monitor frontend. \
             On CI, add a `corepack enable` step before `cargo xtask build-cli` so \
             the packageManager pin in the root package.json activates pnpm. \
             Locally, install with `corepack enable` or `npm install -g pnpm`."
        );
    }

    println!("  → Running pnpm install...");
    let status = Command::new("pnpm")
        .arg("install")
        .current_dir(monitor_dir)
        .status()
        .context("Failed to run pnpm install")?;
    if !status.success() {
        anyhow::bail!("pnpm install failed");
    }

    println!("  → Running pnpm run build...");
    let status = Command::new("pnpm")
        .args(["run", "build"])
        .current_dir(monitor_dir)
        .status()
        .context("Failed to run pnpm run build")?;
    if !status.success() {
        anyhow::bail!("pnpm run build failed");
    }

    let dist_dir = monitor_dir.join("dist");
    if !dist_dir.exists() {
        anyhow::bail!(
            "pnpm run build completed but dist directory not found at {}",
            dist_dir.display()
        );
    }

    println!(
        "✓ Monitor frontend built successfully at {}",
        dist_dir.display()
    );

    Ok(dist_dir)
}

fn pnpm_available() -> bool {
    Command::new("pnpm")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success())
}
