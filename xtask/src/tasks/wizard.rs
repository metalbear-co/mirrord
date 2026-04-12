use std::{
    fs::File,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};
use flate2::{Compression, write::GzEncoder};
use tar::Builder;

/// Builds the wizard frontend (npm install + build)
pub fn build_wizard() -> Result<PathBuf> {
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

    package_wizard()
}

/// Packages the existing wizard frontend dist into the archive embedded by the CLI.
pub fn package_wizard() -> Result<PathBuf> {
    let dist_dir = Path::new("wizard-frontend/dist");

    if !dist_dir.is_dir() {
        anyhow::bail!(
            "Wizard dist directory not found at {}. Run 'cargo xtask build-wizard' first.",
            dist_dir.display()
        );
    }

    let archive_path = Path::new("target")
        .join("wizard")
        .join("wizard-frontend.tar.gz");

    if let Some(parent) = archive_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create wizard archive directory at {}",
                parent.display()
            )
        })?;
    }

    let archive = File::create(&archive_path).with_context(|| {
        format!(
            "Failed to create wizard archive at {}",
            archive_path.display()
        )
    })?;
    let encoder = GzEncoder::new(archive, Compression::default());
    let mut builder = Builder::new(encoder);
    builder
        .append_dir_all(".", dist_dir)
        .with_context(|| format!("Failed to package wizard dist from {}", dist_dir.display()))?;
    let encoder = builder
        .into_inner()
        .context("Failed to finalize wizard tar archive")?;
    encoder
        .finish()
        .context("Failed to finalize wizard gzip archive")?;

    println!("✓ Wizard frontend packaged at {}", archive_path.display());

    Ok(archive_path)
}
