use std::env;

use anyhow::{Context, Result};
use layer::Target;

use super::{cli, layer, wizard};

#[derive(Debug, Clone, Copy)]
pub enum Platform {
    LinuxX86_64,
    LinuxAarch64,
    MacosX86_64,
    MacosAarch64,
    MacosUniversal,
    Windows,
}

impl Platform {
    /// Detect the current platform
    pub fn detect() -> Result<Self> {
        let os = env::consts::OS;
        let arch = env::consts::ARCH;

        match (os, arch) {
            ("macos", _) => Ok(Platform::MacosUniversal),
            ("linux", "x86_64") => Ok(Platform::LinuxX86_64),
            ("linux", "aarch64") => Ok(Platform::LinuxAarch64),
            ("windows", _) => Ok(Platform::Windows),
            _ => anyhow::bail!("Unsupported platform: {} {}", os, arch),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Platform::LinuxX86_64 => "Linux x86_64",
            Platform::LinuxAarch64 => "Linux aarch64",
            Platform::MacosX86_64 => "macOS x86_64",
            Platform::MacosAarch64 => "macOS aarch64",
            Platform::MacosUniversal => "macOS Universal",
            Platform::Windows => "Windows",
        }
    }
}

/// Options for building release CLI
pub struct BuildOptions {
    pub platform: Platform,
    pub release: bool,
    pub with_wizard: bool,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            platform: Platform::detect().unwrap_or(Platform::MacosUniversal),
            release: true,
            with_wizard: true,
        }
    }
}

/// Main task: builds release CLI for the specified platform
pub fn build_release_cli(options: BuildOptions) -> Result<()> {
    println!("════════════════════════════════════════════════════════");
    println!("Building release CLI for {}", options.platform.name());
    println!("  Release mode: {}", options.release);
    println!("  With wizard: {}", options.with_wizard);
    println!("════════════════════════════════════════════════════════");
    println!();

    // Step 1: Build wizard frontend if needed
    if options.with_wizard {
        wizard::build_wizard().context("Failed to build wizard frontend")?;
        println!();
    }

    // Step 2: Build layer and CLI based on platform
    match options.platform {
        Platform::MacosX86_64 => {
            let target = Target::MacosX86_64;
            let layer_path =
                layer::build_layer(target, options.release).context("Failed to build layer")?;
            println!();

            cli::build_cli(target, options.release, &layer_path, options.with_wizard)
                .context("Failed to build CLI")?;
        }
        Platform::MacosAarch64 => {
            let target = Target::MacosAarch64;
            // Build layer
            let layer_path =
                layer::build_layer(target, options.release).context("Failed to build layer")?;
            // Build shim
            layer::build_shim(options.release).context("Failed to build shim")?;
            println!();

            cli::build_cli(target, options.release, &layer_path, options.with_wizard)
                .context("Failed to build CLI")?;
        }
        Platform::MacosUniversal => {
            // Build universal layer
            let layer_path = layer::build_macos_universal_layer(options.release)
                .context("Failed to build macOS universal layer")?;
            println!();

            // Build universal CLI
            cli::build_macos_universal_cli(options.release, &layer_path, options.with_wizard)
                .context("Failed to build macOS universal CLI")?;
        }
        Platform::LinuxX86_64 => {
            let target = Target::LinuxX86_64;
            let layer_path =
                layer::build_layer(target, options.release).context("Failed to build layer")?;
            println!();

            cli::build_cli(target, options.release, &layer_path, options.with_wizard)
                .context("Failed to build CLI")?;
        }
        Platform::LinuxAarch64 => {
            let target = Target::LinuxAarch64;
            // Note: On Linux ARM64, we need to build layer separately from CLI
            // to avoid cross-compilation issues with embedded layer
            let layer_path =
                layer::build_layer(target, options.release).context("Failed to build layer")?;
            println!();

            cli::build_cli(target, options.release, &layer_path, options.with_wizard)
                .context("Failed to build CLI")?;
        }
        Platform::Windows => {
            let target = Target::Windows;
            let layer_path =
                layer::build_layer(target, options.release).context("Failed to build layer")?;
            println!();

            cli::build_cli(target, options.release, &layer_path, options.with_wizard)
                .context("Failed to build CLI")?;
        }
    }

    println!();
    println!("════════════════════════════════════════════════════════");
    println!("✓ Build complete for {}", options.platform.name());
    println!("════════════════════════════════════════════════════════");

    Ok(())
}
