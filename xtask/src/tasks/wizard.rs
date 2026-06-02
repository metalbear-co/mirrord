use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::frontend;

/// Builds the wizard frontend (pnpm install + build).
pub fn build_wizard() -> Result<PathBuf> {
    println!("Building wizard frontend...");

    let wizard_dir = frontend::package_dir("packages/wizard");

    if !wizard_dir.exists() {
        anyhow::bail!("Wizard directory not found at {}.", wizard_dir.display());
    }

    frontend::pnpm_install()?;
    frontend::pnpm_build("mirrord-wizard")?;

    let dist_dir = wizard_dir.join("dist");
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
