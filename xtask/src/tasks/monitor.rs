use std::path::PathBuf;

use anyhow::Result;

use super::frontend;

/// Builds the monitor frontend (pnpm install + build).
///
/// The CLI embeds these assets, so a missing `pnpm` means the resulting binary would have a
/// broken `mirrord ui`.
pub fn build_monitor() -> Result<PathBuf> {
    println!("Building monitor frontend...");

    let monitor_dir = frontend::package_dir("packages/monitor");

    if !monitor_dir.exists() {
        anyhow::bail!("Monitor directory not found at {}.", monitor_dir.display());
    }

    let dist_dir = monitor_dir.join("dist");

    frontend::pnpm_install()?;
    frontend::pnpm_build("session-monitor-frontend")?;

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
