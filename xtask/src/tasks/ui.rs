use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::pnpm;
use crate::relative_to_root;

/// Builds the merged UI frontend (pnpm install + build).
///
/// `packages/ui` is the single site that composes the session monitor and the config wizard. The
/// CLI embeds `packages/ui/dist` via rust-embed, so a missing `pnpm` (and no prebuilt assets) means
/// the resulting binary would have a broken `mirrord ui` / `mirrord wizard`.
pub fn build_ui() -> Result<PathBuf> {
    println!("Building UI frontend...");

    let ui_dir = &relative_to_root(Path::new("packages/ui"));

    if !ui_dir.exists() {
        anyhow::bail!("UI directory not found at {}.", ui_dir.display());
    }

    let dist_dir = ui_dir.join("dist");

    if !pnpm::available_with_corepack_warning() {
        if ui_assets_exist(&dist_dir) {
            println!(
                "  ! `pnpm` is not available; reusing existing UI frontend assets at {}",
                dist_dir.display()
            );
            return Ok(dist_dir);
        }

        anyhow::bail!(
            "`pnpm` not found on PATH. Install it before building the UI frontend \
             (for example with `corepack enable pnpm`)."
        );
    }

    println!("  → Running pnpm --filter mirrord-ui install...");
    let status = pnpm::workspace_command()
        .args(["--filter", "mirrord-ui", "install"])
        .status()
        .context("Failed to run pnpm install")?;
    if !status.success() {
        anyhow::bail!("pnpm install failed");
    }

    println!("  → Running pnpm --filter mirrord-ui run build...");
    let status = pnpm::workspace_command()
        .args(["--filter", "mirrord-ui", "run", "build"])
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

    println!("✓ UI frontend built successfully at {}", dist_dir.display());

    Ok(dist_dir)
}

fn ui_assets_exist(dist_dir: &Path) -> bool {
    dist_dir.join("index.html").is_file() && dist_dir.join("assets").is_dir()
}
