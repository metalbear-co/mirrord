use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};

use crate::relative_to_root;

/// Builds the wizard frontend (npm install + build)
pub fn build_wizard() -> Result<PathBuf> {
    println!("Building wizard frontend...");

    let wizard_dir = &relative_to_root(Path::new("packages/wizard"));

    if !wizard_dir.exists() {
        anyhow::bail!("Wizard directory not found at {}.", wizard_dir.display());
    }

    // npm install
    println!("  → Running npm install...");
    let status = Command::new("npm")
        .arg("install")
        .current_dir(wizard_dir)
        .status()
        .context("Failed to run npm install. Is npm installed?")?;

    if !status.success() {
        anyhow::bail!("npm install failed");
    }

    // npm run build
    println!("  → Running npm run build...");
    let status = Command::new("npm")
        .args(["run", "build"])
        .current_dir(wizard_dir)
        .status()
        .context("Failed to run npm run build")?;

    if !status.success() {
        anyhow::bail!("npm run build failed");
    }

    // Verify dist directory was created
    let dist_dir = wizard_dir.join("dist");
    if !dist_dir.exists() {
        anyhow::bail!(
            "npm run build completed but dist directory not found at {}",
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
