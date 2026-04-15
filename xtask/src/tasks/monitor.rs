use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};

/// Builds the monitor frontend (pnpm install + build).
///
/// If `pnpm` is not available on `PATH`, falls back to creating an empty dist directory so
/// rust-embed doesn't panic at compile time. This keeps CI jobs that don't need the UI
/// (agent image, macos unit tests, e2e runners) from having to install a node toolchain.
/// The resulting binary's `mirrord ui` will 404, which is fine for those contexts.
pub fn build_monitor() -> Result<PathBuf> {
    println!("Building monitor frontend...");

    let monitor_dir = Path::new("packages/monitor");

    if !monitor_dir.exists() {
        anyhow::bail!(
            "Monitor directory not found at {}. Are you in the mirrord repository root?",
            monitor_dir.display()
        );
    }

    let dist_dir = monitor_dir.join("dist");

    if !pnpm_available() {
        eprintln!(
            "warning: `pnpm` not found on PATH — skipping monitor frontend build. \
             `mirrord ui` will return 404 from this binary. Install pnpm (e.g. \
             `corepack enable`) to produce a working UI."
        );
        std::fs::create_dir_all(&dist_dir).with_context(|| {
            format!(
                "failed to create placeholder dist directory at {}",
                dist_dir.display()
            )
        })?;
        return Ok(dist_dir);
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
