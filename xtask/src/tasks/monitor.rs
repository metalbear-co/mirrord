use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::pnpm;
use crate::relative_to_root;

/// Builds the monitor frontend (pnpm install + build).
///
/// The CLI embeds these assets, so a missing `pnpm` means the resulting binary would have a
/// broken `mirrord ui`.
pub fn build_monitor() -> Result<PathBuf> {
    println!("Building monitor frontend...");

    let monitor_dir = &relative_to_root(Path::new("packages/monitor"));

    if !monitor_dir.exists() {
        anyhow::bail!("Monitor directory not found at {}.", monitor_dir.display());
    }

    let dist_dir = monitor_dir.join("dist");

    if !pnpm::available_with_corepack_warning() {
        if monitor_assets_exist(&dist_dir) {
            println!(
                "  ! `pnpm` is not available; reusing existing monitor frontend assets at {}",
                dist_dir.display()
            );
            return Ok(dist_dir);
        }

        anyhow::bail!(
            "`pnpm` not found on PATH. Install it before building the monitor frontend \
             (for example with `corepack enable pnpm`)."
        );
    }

    println!("  → Running pnpm --filter session-monitor-frontend install...");
    let status = pnpm::workspace_command()
        .args(["--filter", "session-monitor-frontend", "install"])
        .status()
        .context("Failed to run pnpm install")?;
    if !status.success() {
        anyhow::bail!("pnpm install failed");
    }

    println!("  → Running pnpm --filter session-monitor-frontend run build...");
    let status = pnpm::workspace_command()
        .args(["--filter", "session-monitor-frontend", "run", "build"])
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

fn monitor_assets_exist(dist_dir: &Path) -> bool {
    dist_dir.join("index.html").is_file() && dist_dir.join("assets").is_dir()
}
