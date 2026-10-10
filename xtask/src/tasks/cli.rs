use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use mirrord_command::resolve_command;

use super::{
    layer::{self, CargoOptions, Target},
    signing, sip_binaries,
};
use crate::relative_to_root;

/// Builds the mirrord CLI for the specified target.
///
/// The merged UI frontend (`packages/ui/dist`) is embedded via rust-embed at compile time, so it
/// only needs to exist on disk before this runs (see [`super::ui::build_ui`]); there is no build
/// feature or env var to wire up here.
pub fn build_cli(
    target: Target,
    options: CargoOptions,
    layer_path: &Path,
    cargo_args: &[String],
) -> Result<PathBuf> {
    println!("Building mirrord CLI for {}...", target.triple());

    let mut cmd = layer::cargo_build(target, options);
    cmd.arg("-p").arg("mirrord");

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

        let arm_layer = Target::MacosAarch64.layer_file(options);
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

    let binary_name = if matches!(target, Target::Windows) {
        "mirrord.exe"
    } else {
        "mirrord"
    };

    let cli_path = relative_to_root(&target.out_dir(options).join(binary_name));

    println!("✓ CLI built: {}", cli_path.display());
    Ok(cli_path)
}

/// Merges pre-built architecture-specific CLIs into universal binary
pub fn merge_macos_universal_cli(release: bool) -> Result<PathBuf> {
    println!("Merging macOS universal CLI from pre-built architectures...");

    let options = CargoOptions {
        release,
        ..Default::default()
    };

    // Check that CLIs exist
    let x86_cli = Target::MacosX86_64.out_dir(options).join("mirrord");
    let arm_cli = Target::MacosAarch64.out_dir(options).join("mirrord");

    if !x86_cli.exists() {
        anyhow::bail!("x86_64 CLI not found at {}", x86_cli.display());
    }
    if !arm_cli.exists() {
        anyhow::bail!("aarch64 CLI not found at {}", arm_cli.display());
    }

    // Create universal directory
    let universal_dir = Target::MacosUniversal.out_dir(options);
    std::fs::create_dir_all(&universal_dir).context("Failed to create universal directory")?;

    // Create universal binary with lipo
    let universal_cli = universal_dir.join("mirrord");
    println!("Creating universal CLI with lipo...");

    let status = resolve_command("lipo")
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
    options: CargoOptions,
    universal_layer_path: &Path,
    cargo_args: &[String],
) -> Result<PathBuf> {
    println!("Building macOS universal CLI...");

    // Build both architectures
    let x86_cli = build_cli(
        Target::MacosX86_64,
        options,
        universal_layer_path,
        cargo_args,
    )?;
    let arm_cli = build_cli(
        Target::MacosAarch64,
        options,
        universal_layer_path,
        cargo_args,
    )?;

    // Sign architecture-specific CLIs (can batch sign with gon in CI)
    signing::sign_binaries(&[x86_cli.clone(), arm_cli.clone()])?;

    // Create universal binary
    let universal_dir = Target::MacosUniversal.out_dir(options);
    std::fs::create_dir_all(&universal_dir).context("Failed to create universal directory")?;

    let universal_cli = universal_dir.join("mirrord");
    println!("Creating universal CLI...");
    let status = resolve_command("lipo")
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
