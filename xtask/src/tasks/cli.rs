use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};
use which::which;

use super::{layer::Target, signing, sip_binaries};

/// Builds the mirrord CLI for the specified target
pub fn build_cli(
    target: Target,
    release: bool,
    layer_path: &Path,
    wizard_dist: Option<&Path>,
    cargo_args: &[String],
) -> Result<PathBuf> {
    println!("Building mirrord CLI for {}...", target.triple());

    let is_linux = matches!(target, Target::LinuxX86_64 | Target::LinuxAarch64);

    if is_linux && which("cargo-zigbuild").is_err() {
        anyhow::bail!("cargo-zigbuild is required for Linux builds.");
    }

    let mut cmd = Command::new("cargo");
    if is_linux {
        cmd.arg("zigbuild");
    } else {
        cmd.arg("build");
    }
    cmd.arg("-p").arg("mirrord");

    if release {
        cmd.arg("--release");
    }

    let target_triple = if is_linux {
        format!("{}.2.17", target.triple())
    } else {
        target.triple().to_owned()
    };
    cmd.arg("--target").arg(&target_triple);

    if let Some(wizard_dist) = wizard_dist {
        cmd.arg("--features").arg("wizard");

        if !wizard_dist.is_dir() {
            anyhow::bail!(
                "Wizard dist directory not found at {}. Run 'cargo xtask build-wizard' first.",
                wizard_dist.display()
            );
        }

        cmd.env(
            "WIZARD_DIST_DIR",
            wizard_dist
                .canonicalize()
                .context("Failed to canonicalize wizard dist path")?,
        );
    }

    // Set layer file environment variable
    cmd.env(
        "MIRRORD_LAYER_FILE",
        layer_path
            .canonicalize()
            .context("Failed to canonicalize layer path")?,
    );

    // For macOS builds, also set the ARM64 layer path
    if matches!(
        target,
        Target::MacosX86_64 | Target::MacosAarch64 | Target::MacosUniversal
    ) {
        let sip_binaries_archive = sip_binaries::download()?;
        cmd.env(
            "MIRRORD_SIP_BINARIES_TAR",
            sip_binaries_archive
                .canonicalize()
                .context("Failed to canonicalize SIP utilities bundle path")?,
        );

        let mode = if release { "release" } else { "debug" };
        let arm_layer = Path::new("target")
            .join("aarch64-apple-darwin")
            .join(mode)
            .join("libmirrord_layer.dylib");
        cmd.env(
            "MIRRORD_LAYER_FILE_MACOS_ARM64",
            arm_layer
                .canonicalize()
                .context("Failed to canonicalize ARM64 layer path")?,
        );
    }

    cmd.args(cargo_args);

    let status = cmd.status().context("Failed to run cargo build")?;

    if !status.success() {
        anyhow::bail!("cargo build failed for {}", target.triple());
    }

    let mode = if release { "release" } else { "debug" };
    let binary_name = if matches!(target, Target::Windows) {
        "mirrord.exe"
    } else {
        "mirrord"
    };

    let cli_path = Path::new("target")
        .join(target.triple())
        .join(mode)
        .join(binary_name);

    println!("✓ CLI built: {}", cli_path.display());
    Ok(cli_path)
}

/// Merges pre-built architecture-specific CLIs into universal binary
pub fn merge_macos_universal_cli(release: bool) -> Result<PathBuf> {
    println!("Merging macOS universal CLI from pre-built architectures...");

    let mode = if release { "release" } else { "debug" };

    // Check that CLIs exist
    let x86_cli = Path::new("target/x86_64-apple-darwin")
        .join(mode)
        .join("mirrord");
    let arm_cli = Path::new("target/aarch64-apple-darwin")
        .join(mode)
        .join("mirrord");

    if !x86_cli.exists() {
        anyhow::bail!("x86_64 CLI not found at {}", x86_cli.display());
    }
    if !arm_cli.exists() {
        anyhow::bail!("aarch64 CLI not found at {}", arm_cli.display());
    }

    // Create universal directory
    let universal_dir = Path::new("target/universal-apple-darwin").join(mode);
    std::fs::create_dir_all(&universal_dir).context("Failed to create universal directory")?;

    // Create universal binary with lipo
    let universal_cli = universal_dir.join("mirrord");
    println!("Creating universal CLI with lipo...");

    let status = Command::new("lipo")
        .args(["-create", "-output"])
        .arg(&universal_cli)
        .arg(&x86_cli)
        .arg(&arm_cli)
        .status()
        .context("Failed to create universal binary")?;

    if !status.success() {
        anyhow::bail!("lipo failed");
    }

    // Sign universal CLI
    signing::sign_binary(&universal_cli)?;

    println!("✓ Universal CLI merged: {}", universal_cli.display());
    Ok(universal_cli)
}

/// Builds the macOS universal CLI (combines x86_64 and aarch64)
pub fn build_macos_universal_cli(
    release: bool,
    universal_layer_path: &Path,
    wizard_dist: Option<&Path>,
    cargo_args: &[String],
) -> Result<PathBuf> {
    println!("Building macOS universal CLI...");

    // Build both architectures
    let x86_cli = build_cli(
        Target::MacosX86_64,
        release,
        universal_layer_path,
        wizard_dist,
        cargo_args,
    )?;
    let arm_cli = build_cli(
        Target::MacosAarch64,
        release,
        universal_layer_path,
        wizard_dist,
        cargo_args,
    )?;

    // Sign architecture-specific CLIs (can batch sign with gon in CI)
    signing::sign_binaries(&[x86_cli.clone(), arm_cli.clone()])?;

    // Create universal binary
    let mode = if release { "release" } else { "debug" };
    let universal_dir = Path::new("target/universal-apple-darwin").join(mode);
    std::fs::create_dir_all(&universal_dir).context("Failed to create universal directory")?;

    let universal_cli = universal_dir.join("mirrord");
    println!("Creating universal CLI...");
    let status = Command::new("lipo")
        .args(["-create", "-output"])
        .arg(&universal_cli)
        .arg(&x86_cli)
        .arg(&arm_cli)
        .status()
        .context("Failed to create universal binary")?;

    if !status.success() {
        anyhow::bail!("lipo failed");
    }

    // Sign universal CLI
    signing::sign_binary(&universal_cli)?;

    println!("✓ Universal CLI built: {}", universal_cli.display());
    Ok(universal_cli)
}
