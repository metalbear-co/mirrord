use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};

use super::signing;

/// Target platform for building
#[derive(Debug, Clone, Copy)]
pub enum Target {
    LinuxX86_64,
    LinuxAarch64,
    MacosX86_64,
    MacosAarch64,
    #[allow(dead_code)]
    MacosUniversal,
    Windows,
}

impl Target {
    pub fn triple(&self) -> &str {
        match self {
            Target::LinuxX86_64 => "x86_64-unknown-linux-gnu",
            Target::LinuxAarch64 => "aarch64-unknown-linux-gnu",
            Target::MacosX86_64 => "x86_64-apple-darwin",
            Target::MacosAarch64 => "aarch64-apple-darwin",
            Target::MacosUniversal => "universal-apple-darwin",
            Target::Windows => "x86_64-pc-windows-msvc",
        }
    }

    pub fn layer_file(&self, release: bool) -> PathBuf {
        let mode = if release { "release" } else { "debug" };
        let ext = match self {
            Target::Windows => "dll",
            Target::MacosX86_64 | Target::MacosAarch64 | Target::MacosUniversal => "dylib",
            _ => "so",
        };
        let name = match self {
            Target::Windows => "mirrord_layer_win",
            _ => "libmirrord_layer",
        };

        Path::new("target")
            .join(self.triple())
            .join(mode)
            .join(format!("{}.{}", name, ext))
    }
}

/// Builds the mirrord layer for the specified target
pub fn build_layer(target: Target, release: bool) -> Result<PathBuf> {
    // Special case: MacosUniversal needs to build both architectures + shim
    if matches!(target, Target::MacosUniversal) {
        return build_macos_universal_layer(release);
    }

    println!("Building mirrord-layer for {}...", target.triple());

    let mut cmd = Command::new("cargo");
    cmd.arg("build").arg("-p");

    match target {
        Target::Windows => cmd.arg("mirrord-layer-win"),
        _ => cmd.arg("mirrord-layer"),
    };

    if release {
        cmd.arg("--release");
    }

    cmd.arg("--target").arg(target.triple());

    let status = cmd.status().context("Failed to run cargo build")?;

    if !status.success() {
        anyhow::bail!("cargo build failed for {}", target.triple());
    }

    let layer_path = target.layer_file(release);
    println!("✓ Layer built: {}", layer_path.display());
    Ok(layer_path)
}

/// Builds the arm64e shim for macOS
pub fn build_shim(release: bool) -> Result<PathBuf> {
    let mode = if release { "release" } else { "debug" };
    let shim_dir = Path::new("target/aarch64-apple-darwin").join(mode);
    std::fs::create_dir_all(&shim_dir).context("Failed to create shim directory")?;

    let shim_path = shim_dir.join("shim.dylib");
    println!("Building arm64e shim...");

    let status = Command::new("clang")
        .args(["-arch", "arm64e", "-dynamiclib", "-o"])
        .arg(&shim_path)
        .arg("mirrord/layer/shim.c")
        .status()
        .context("Failed to build shim")?;

    if !status.success() {
        anyhow::bail!("Failed to build shim");
    }

    // Sign shim
    signing::sign_binary(&shim_path)?;

    println!("✓ Shim built: {}", shim_path.display());
    Ok(shim_path)
}

/// Links pre-built architecture-specific layers into universal binary
pub fn link_macos_universal_layer(release: bool) -> Result<PathBuf> {
    println!("Linking macOS universal layer from pre-built architectures...");

    let mode = if release { "release" } else { "debug" };

    // Check that all required files exist
    let x86_layer = Path::new("target/x86_64-apple-darwin")
        .join(mode)
        .join("libmirrord_layer.dylib");
    let arm_layer = Path::new("target/aarch64-apple-darwin")
        .join(mode)
        .join("libmirrord_layer.dylib");
    let shim_path = Path::new("target/aarch64-apple-darwin")
        .join(mode)
        .join("shim.dylib");

    if !x86_layer.exists() {
        anyhow::bail!("x86_64 layer not found at {}", x86_layer.display());
    }
    if !arm_layer.exists() {
        anyhow::bail!("aarch64 layer not found at {}", arm_layer.display());
    }
    if !shim_path.exists() {
        anyhow::bail!("shim not found at {}", shim_path.display());
    }

    // Create universal directory
    let universal_dir = Path::new("target/universal-apple-darwin").join(mode);
    std::fs::create_dir_all(&universal_dir).context("Failed to create universal directory")?;

    // Create universal dylib with lipo
    let universal_layer = universal_dir.join("libmirrord_layer.dylib");
    println!("Creating universal dylib with lipo...");

    let status = Command::new("lipo")
        .args(["-create", "-output"])
        .arg(&universal_layer)
        .arg(&x86_layer)
        .arg(&shim_path)
        .arg(&arm_layer)
        .status()
        .context("Failed to create universal binary")?;

    if !status.success() {
        anyhow::bail!("lipo failed");
    }

    // Sign universal layer
    signing::sign_binary(&universal_layer)?;

    println!("✓ Universal layer linked: {}", universal_layer.display());
    Ok(universal_layer)
}

/// Builds the macOS universal layer (combines x86_64, aarch64, and shim)
pub fn build_macos_universal_layer(release: bool) -> Result<PathBuf> {
    println!("Building macOS universal layer...");

    // Build both architectures
    let x86_layer = build_layer(Target::MacosX86_64, release)?;
    let arm_layer = build_layer(Target::MacosAarch64, release)?;

    // Build shim
    let shim_path = build_shim(release)?;

    // Sign architecture-specific layers (can batch sign with gon in CI)
    signing::sign_binaries(&[x86_layer.clone(), arm_layer.clone(), shim_path.clone()])?;

    // Create universal directory
    let mode = if release { "release" } else { "debug" };
    let universal_dir = Path::new("target/universal-apple-darwin").join(mode);
    std::fs::create_dir_all(&universal_dir).context("Failed to create universal directory")?;

    // Create universal dylib
    let universal_layer = universal_dir.join("libmirrord_layer.dylib");
    println!("Creating universal dylib...");
    let status = Command::new("lipo")
        .args(["-create", "-output"])
        .arg(&universal_layer)
        .arg(&x86_layer)
        .arg(&shim_path)
        .arg(&arm_layer)
        .status()
        .context("Failed to create universal binary")?;

    if !status.success() {
        anyhow::bail!("lipo failed");
    }

    // Sign universal layer
    signing::sign_binary(&universal_layer)?;

    println!("✓ Universal layer built: {}", universal_layer.display());
    Ok(universal_layer)
}
