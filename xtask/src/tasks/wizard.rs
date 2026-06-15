use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::pnpm;
use crate::relative_to_root;

/// Builds the wizard frontend (pnpm install + build)
pub fn build_wizard() -> Result<PathBuf> {
    println!("Building wizard frontend...");

    let wizard_dir = &relative_to_root(Path::new("packages/wizard"));

    if !wizard_dir.exists() {
        anyhow::bail!("Wizard directory not found at {}.", wizard_dir.display());
    }

    let dist_dir = wizard_dir.join("dist");

    if !pnpm::available_with_corepack_warning() {
        if frontend_assets_exist(&dist_dir) {
            println!(
                "  ! `pnpm` is not available; reusing existing wizard frontend assets at {}",
                dist_dir.display()
            );
            return Ok(dist_dir);
        }

        anyhow::bail!(
            "`pnpm` not found on PATH. Install it before building the wizard frontend \
             (for example with `corepack enable pnpm`)."
        );
    }

    println!("  → Running pnpm --filter mirrord-wizard install...");
    let status = pnpm::workspace_command()
        .args(["--filter", "mirrord-wizard", "install"])
        .status()
        .context("Failed to run pnpm install")?;

    if !status.success() {
        anyhow::bail!("pnpm install failed");
    }

    println!("  → Running pnpm --filter mirrord-wizard run build...");
    let status = pnpm::workspace_command()
        .args(["--filter", "mirrord-wizard", "run", "build"])
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
        "✓ Wizard frontend built successfully at {}",
        dist_dir.display()
    );

    Ok(dist_dir)
}

/// Resolves the existing wizard frontend dist directory embedded by the CLI build script.
pub fn package_wizard() -> Result<PathBuf> {
    let dist_dir = Path::new("packages/wizard/dist");

    if !dist_dir.is_dir() {
        anyhow::bail!(
            "Wizard dist directory not found at {}. Run 'cargo xtask build-wizard' first.",
            dist_dir.display()
        );
    }

    dist_dir.canonicalize().with_context(|| {
        format!(
            "Failed to canonicalize wizard dist directory at {}",
            dist_dir.display()
        )
    })
}

fn frontend_assets_exist(dist_dir: &Path) -> bool {
    dist_dir.join("index.html").is_file() && dist_dir.join("assets").is_dir()
}
