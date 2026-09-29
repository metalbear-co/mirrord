use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};
use which::which;

use super::signing;
use crate::relative_to_root;

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

    /// The triple to pass to cargo with `--target`, if any.
    ///
    /// With `--target`, cargo puts the artifacts in `target/<triple>/` and does not share its cache
    /// with a plain `cargo build` or `cargo clippy`. So xtask passes it only when `cross` is set,
    /// that is, when the platform is given explicitly, or when the target is not the host
    /// platform.
    ///
    /// macOS is different: a universal build compiles both architectures on one host, so only the
    /// architecture that is not the host gets `--target`, even when the platform is given
    /// explicitly. The universal target is not built by cargo, but its directory is named the same
    /// way.
    fn cargo_target(&self, cross: bool) -> Option<&str> {
        let native = matches!(
            (self, env::consts::OS, env::consts::ARCH),
            (Target::LinuxX86_64, "linux", "x86_64")
                | (Target::LinuxAarch64, "linux", "aarch64")
                | (Target::MacosX86_64, "macos", "x86_64")
                | (Target::MacosAarch64, "macos", "aarch64")
                | (Target::Windows, "windows", "x86_64")
        );

        match self {
            Target::MacosX86_64 | Target::MacosAarch64 if native => None,
            _ if cross || !native => Some(self.triple()),
            _ => None,
        }
    }

    /// The directory where cargo puts the artifacts for this target.
    pub fn out_dir(&self, options: CargoOptions) -> PathBuf {
        let mode = if options.release { "release" } else { "debug" };
        match self.cargo_target(options.cross) {
            Some(triple) => Path::new("target").join(triple).join(mode),
            None => Path::new("target").join(mode),
        }
    }

    pub fn layer_file(&self, options: CargoOptions) -> PathBuf {
        let ext = match self {
            Target::Windows => "dll",
            Target::MacosX86_64 | Target::MacosAarch64 | Target::MacosUniversal => "dylib",
            _ => "so",
        };
        let name = match self {
            Target::Windows => "mirrord_layer_win",
            _ => "libmirrord_layer",
        };

        self.out_dir(options).join(format!("{}.{}", name, ext))
    }
}

/// Options for the cargo builds of the layer and the CLI.
#[derive(Debug, Clone, Copy, Default)]
pub struct CargoOptions {
    pub release: bool,
    /// The platform was given explicitly, see [`Target::cargo_target`].
    pub cross: bool,
}

/// Creates the cargo command that builds `target`. Callers add the package and extra arguments.
///
/// Linux targets use `cargo zigbuild`, which links against glibc 2.17 so the binaries also run on
/// old distros.
pub fn cargo_build(target: Target, options: CargoOptions) -> Result<Command> {
    let zigbuild = matches!(target, Target::LinuxX86_64 | Target::LinuxAarch64);
    if zigbuild && which("cargo-zigbuild").is_err() {
        anyhow::bail!("cargo-zigbuild is required for Linux builds.");
    }

    let mut cmd = Command::new("cargo");
    cmd.arg(if zigbuild { "zigbuild" } else { "build" });

    if options.release {
        cmd.arg("--release");
    }

    match target.cargo_target(options.cross) {
        // `cargo zigbuild` takes the glibc version to link against from the target triple.
        Some(triple) if zigbuild => cmd.arg("--target").arg(format!("{triple}.2.17")),
        Some(triple) => cmd.arg("--target").arg(triple),
        None => &mut cmd,
    };

    Ok(cmd)
}

/// Builds the mirrord layer for the specified target
pub fn build_layer(
    target: Target,
    options: CargoOptions,
    cargo_args: &[String],
) -> Result<PathBuf> {
    // Special case: MacosUniversal needs to build both architectures + shim
    if matches!(target, Target::MacosUniversal) {
        return build_macos_universal_layer(options, cargo_args);
    }

    println!("Building mirrord-layer for {}...", target.triple());

    let mut cmd = cargo_build(target, options)?;
    cmd.arg("-p");

    match target {
        Target::Windows => cmd.arg("mirrord-layer-win"),
        _ => cmd.arg("mirrord-layer"),
    };

    cmd.args(cargo_args);

    let status = cmd.status().context("Failed to run cargo build")?;

    if !status.success() {
        anyhow::bail!("cargo build failed for {}", target.triple());
    }

    let layer_path = relative_to_root(&target.layer_file(options));
    println!("✓ Layer built: {}", layer_path.display());
    Ok(layer_path)
}

/// Builds the arm64e shim for macOS
pub fn build_shim(options: CargoOptions) -> Result<PathBuf> {
    let shim_dir = Target::MacosAarch64.out_dir(options);
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

    let options = CargoOptions {
        release,
        ..Default::default()
    };

    // Check that all required files exist
    let x86_layer = Target::MacosX86_64.layer_file(options);
    let arm_layer = Target::MacosAarch64.layer_file(options);
    let shim_path = Target::MacosAarch64.out_dir(options).join("shim.dylib");

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
    let universal_dir = Target::MacosUniversal.out_dir(options);
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
pub fn build_macos_universal_layer(
    options: CargoOptions,
    cargo_args: &[String],
) -> Result<PathBuf> {
    println!("Building macOS universal layer...");

    // Build both architectures
    let x86_layer = build_layer(Target::MacosX86_64, options, cargo_args)?;
    let arm_layer = build_layer(Target::MacosAarch64, options, cargo_args)?;

    // Build shim
    let shim_path = build_shim(options)?;

    // Sign architecture-specific layers (can batch sign with gon in CI)
    signing::sign_binaries(&[x86_layer.clone(), arm_layer.clone(), shim_path.clone()])?;

    // Create universal directory
    let universal_dir = Target::MacosUniversal.out_dir(options);
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
