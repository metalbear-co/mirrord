use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};

use crate::relative_to_root;

/// Builds the monitor frontend (pnpm install + build).
///
/// The CLI embeds these assets, so a missing `pnpm` means the resulting binary would have a
/// broken `mirrord ui`.
pub fn build_monitor() -> Result<PathBuf> {
    println!("Building monitor frontend...");

    let monitor_dir = &relative_to_root(Path::new("packages/monitor"));

    if !monitor_dir.exists() {
        anyhow::bail!(
            "Monitor directory not found at {}.",
            monitor_dir.display()
        );
    }

    if !pnpm_available() {
        anyhow::bail!(
            "`pnpm` not found on PATH. Install it before building the monitor frontend \
             (for example with `corepack enable pnpm`)."
        );
    }

    let dist_dir = monitor_dir.join("dist");

    // `--ignore-workspace` stops pnpm from walking up to the repo root and installing for all
    // workspaces (the root `package.json` has `workspaces: ["packages/*"]`). Without it, pnpm
    // touches `packages/wizard/node_modules` in a way that breaks the subsequent
    // `npm install` run by `build_wizard()` ("Cannot read properties of null (reading
    // 'matches')"). Monitor only depends on its own package.json, so a scoped install is
    // what we want.
    println!("  → Running pnpm install --ignore-workspace...");
    let status = Command::new("pnpm")
        .args(["install", "--ignore-workspace"])
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

    if !dist_dir.exists() {
        anyhow::bail!(
            "pnpm run build completed but dist directory not found at {}",
            dist_dir.display()
        );
    }

    // Remove `packages/monitor/node_modules` after the dist is built. rust-embed only
    // consumes `packages/monitor/dist/`. Leaving the pnpm-installed `node_modules` around
    // breaks the wizard's later `npm install` — npm walks up to the repo root's
    // `workspaces: ["packages/*"]`, tries to dedupe against the monitor's pnpm-style tree,
    // and crashes with `Cannot read properties of null (reading 'matches')`.
    let node_modules = monitor_dir.join("node_modules");
    if node_modules.exists() {
        std::fs::remove_dir_all(&node_modules).with_context(|| {
            format!(
                "failed to remove {} after monitor build",
                node_modules.display()
            )
        })?;
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
