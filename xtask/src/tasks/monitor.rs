use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};

/// Builds the monitor frontend (pnpm install + build).
pub fn build_monitor() -> Result<PathBuf> {
    println!("Building monitor frontend...");

    let monitor_dir = Path::new("packages/monitor");

    if !monitor_dir.exists() {
        anyhow::bail!(
            "Monitor directory not found at {}. Are you in the mirrord repository root?",
            monitor_dir.display()
        );
    }

    // pnpm install
    println!("  → Running pnpm install...");
    let status = Command::new("pnpm")
        .arg("install")
        .current_dir(monitor_dir)
        .status()
        .context("Failed to run pnpm install. Is pnpm installed?")?;

    if !status.success() {
        anyhow::bail!("pnpm install failed");
    }

    // pnpm run build
    println!("  → Running pnpm run build...");
    let status = Command::new("pnpm")
        .args(["run", "build"])
        .current_dir(monitor_dir)
        .status()
        .context("Failed to run pnpm run build")?;

    if !status.success() {
        anyhow::bail!("pnpm run build failed");
    }

    // Verify dist directory was created
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
