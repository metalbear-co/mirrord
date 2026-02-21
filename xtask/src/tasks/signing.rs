use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};

/// Check if we should use gon for signing (CI environment)
fn should_use_gon() -> bool {
    env::var("AC_USERNAME").is_ok() && env::var("AC_PASSWORD").is_ok()
}

/// Signs a binary using either gon (CI) or codesign (local dev)
pub fn sign_binary(path: &Path) -> Result<()> {
    if should_use_gon() {
        sign_with_gon(&[path])
    } else {
        sign_with_codesign(path)
    }
}

/// Signs multiple binaries using either gon (CI) or codesign (local dev)
pub fn sign_binaries(paths: &[PathBuf]) -> Result<()> {
    if should_use_gon() {
        sign_with_gon(paths)
    } else {
        for path in paths {
            sign_with_codesign(path)?;
        }
        Ok(())
    }
}

/// Sign using gon (for CI with Apple Developer credentials)
fn sign_with_gon(paths: &[impl AsRef<Path>]) -> Result<()> {
    println!("Signing with gon (CI mode)...");

    // Create temporary gon config
    let config = create_gon_config(paths)?;
    let config_path = Path::new("/tmp/xtask_gon_config.json");
    std::fs::write(config_path, config).context("Failed to write gon config")?;

    // Run gon
    let status = Command::new("gon")
        .args(["-log-level=debug", "-log-json"])
        .arg(config_path)
        .status()
        .context("Failed to run gon. Is gon installed? (brew install mitchellh/gon/gon)")?;

    if !status.success() {
        anyhow::bail!("gon signing failed");
    }

    // Clean up
    std::fs::remove_file(config_path).ok();

    println!("✓ Signed with gon");
    Ok(())
}

/// Sign using simple codesign (for local development)
fn sign_with_codesign(path: &Path) -> Result<()> {
    println!("Signing {} with codesign (local mode)...", path.display());

    let status = Command::new("codesign")
        .args(["-f", "-s", "-"])
        .arg(path)
        .status()
        .context("Failed to run codesign")?;

    if !status.success() {
        anyhow::bail!("codesign failed for {}", path.display());
    }

    Ok(())
}

/// Create gon configuration JSON
fn create_gon_config(paths: &[impl AsRef<Path>]) -> Result<String> {
    let sources: Vec<String> = paths
        .iter()
        .map(|p| {
            let path = p.as_ref();
            // Convert to absolute path string
            path.canonicalize()
                .unwrap_or_else(|_| path.to_path_buf())
                .to_string_lossy()
                .into_owned()
        })
        .collect();

    let config = serde_json::json!({
        "source": sources,
        "bundle_id": "com.metalbear.mirrord",
        "apple_id": {
            "username": "@env:AC_USERNAME",
            "password": "@env:AC_PASSWORD"
        },
        "sign": {
            "application_identity": "Developer ID Application: METALBEAR TECH LTD (8W42TQ6PFA)"
        }
    });

    serde_json::to_string_pretty(&config).context("Failed to serialize gon config")
}
