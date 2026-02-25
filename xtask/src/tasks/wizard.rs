use std::{path::Path, process::Command};

use anyhow::{Context, Result};

/// Builds the wizard frontend (npm install + build)
pub fn build_wizard() -> Result<()> {
    println!("Building wizard frontend...");

    let wizard_dir = Path::new("wizard-frontend");

    if !wizard_dir.exists() {
        anyhow::bail!(
            "Wizard directory not found at {}. Are you in the mirrord repository root?",
            wizard_dir.display()
        );
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
    Ok(())
}
