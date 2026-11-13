use std::{
    env,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use mirrord_command::resolve_command;

use super::layer::{self, CargoOptions, Target};

#[derive(Clone, Copy)]
pub enum Suite {
    E2e,
    Integration,
}

impl Suite {
    fn name(self) -> &'static str {
        match self {
            Suite::E2e => "e2e",
            Suite::Integration => "integration",
        }
    }

    fn package(self) -> &'static str {
        match self {
            Suite::E2e => "mirrord-tests",
            Suite::Integration => "mirrord-layer-tests",
        }
    }
}

pub fn run(
    suite: Suite,
    mirrord_binary: Option<PathBuf>,
    mirrord_layer: Option<PathBuf>,
    cargo_args: Vec<String>,
) -> Result<()> {
    let (mirrord_binary, mirrord_layer) = resolve_artifacts(mirrord_binary, mirrord_layer)?;

    println!(
        "Running {} tests with mirrord binary {} and layer {}",
        suite.name(),
        mirrord_binary.display(),
        mirrord_layer.display()
    );

    let mut cmd = resolve_command("cargo");

    cmd.args(["nextest", "run"]);

    cmd.arg("-p").arg(suite.package());

    if !cargo_args.is_empty() {
        cmd.args(cargo_args);
    }

    cmd.env("MIRRORD_TESTS_USE_BINARY", &mirrord_binary);
    cmd.env("MIRRORD_LAYER_FILE", &mirrord_layer);

    let status = cmd.status().context("Failed to run cargo test command")?;

    if !status.success() {
        bail!("{} tests failed", suite.name());
    }

    Ok(())
}

/// Resolves the mirrord binary and layer paths.
///
/// Uses CLI args take precendence over envvars. If neither is provided, then build automatically
/// from source. test-appropriate defaults.
fn resolve_artifacts(
    binary_arg: Option<PathBuf>,
    layer_arg: Option<PathBuf>,
) -> Result<(PathBuf, PathBuf)> {
    let binary_from_env = env::var_os("MIRRORD_TESTS_USE_BINARY").map(PathBuf::from);
    let layer_from_env = env::var_os("MIRRORD_LAYER_FILE").map(PathBuf::from);

    let layer = match layer_arg.or(layer_from_env) {
        Some(layer) => validate_path(&layer, "MIRRORD_LAYER_FILE", "mirrord layer")?,
        None => build_layer_for_tests()?,
    };

    let binary = match binary_arg.or(binary_from_env) {
        Some(binary) => validate_path(&binary, "MIRRORD_TESTS_USE_BINARY", "mirrord binary")?,
        None => build_binary_for_tests(&layer)?,
    };

    Ok((binary, layer))
}

fn host_cli_target() -> Result<Target> {
    match (env::consts::OS, env::consts::ARCH) {
        ("macos", "x86_64") => Ok(Target::MacosX86_64),
        ("macos", "aarch64") => Ok(Target::MacosAarch64),
        ("linux", "x86_64") => Ok(Target::LinuxX86_64),
        ("linux", "aarch64") => Ok(Target::LinuxAarch64),
        ("windows", _) => Ok(Target::Windows),
        (os, arch) => bail!("Unsupported host platform: {os} {arch}"),
    }
}

fn test_layer_target() -> Result<Target> {
    match (env::consts::OS, env::consts::ARCH) {
        ("macos", _) => Ok(Target::MacosUniversal),
        ("linux", "x86_64") => Ok(Target::LinuxX86_64),
        ("linux", "aarch64") => Ok(Target::LinuxAarch64),
        ("windows", _) => Ok(Target::Windows),
        (os, arch) => bail!("Unsupported host platform: {os} {arch}"),
    }
}

fn build_layer_for_tests() -> Result<PathBuf> {
    let target = test_layer_target()?;
    let layer = layer::build_layer(target, CargoOptions::default(), &[])?;
    canonicalize_built_artifact(&layer, "built mirrord layer")
}

fn build_binary_for_tests(layer_path: &Path) -> Result<PathBuf> {
    let target = host_cli_target()?;
    ensure_macos_arm64_layer(target)?;
    super::ui::build_ui()?;
    let binary = super::cli::build_cli(target, CargoOptions::default(), layer_path, &[])?;
    canonicalize_built_artifact(&binary, "built mirrord binary")
}

fn ensure_macos_arm64_layer(target: Target) -> Result<()> {
    if !matches!(target, Target::MacosX86_64 | Target::MacosAarch64) {
        return Ok(());
    }

    let arm64_layer = Target::MacosAarch64.layer_file(CargoOptions::default());

    if !arm64_layer.is_file() {
        layer::build_layer(Target::MacosAarch64, CargoOptions::default(), &[])?;
    }

    Ok(())
}

fn canonicalize_built_artifact(path: &Path, description: &str) -> Result<PathBuf> {
    path.canonicalize()
        .with_context(|| format!("Failed to canonicalize {description}: {}", path.display()))
}

/// Runs CLI unit tests with the placeholder embedded files from `.cargo/config.toml`.
///
/// The unit tests exercise argument parsing and other logic, not the embedded layer or UI, so the
/// test job doesn't depend on the frontend and layer build steps.
pub fn run_unit(cargo_args: Vec<String>) -> Result<()> {
    let mut cmd = resolve_command("cargo");
    cmd.args(["nextest", "run", "-p", "mirrord-cli"]);
    cmd.args(cargo_args);

    let status = cmd.status().context("Failed to run cargo test command")?;
    if !status.success() {
        bail!("unit tests failed");
    }

    Ok(())
}

fn validate_path(path: &Path, env_name: &str, description: &str) -> Result<PathBuf> {
    if !path.is_file() {
        bail!(
            "{env_name} must point to an existing {description}, got {}",
            path.display()
        );
    }

    path.canonicalize().with_context(|| {
        format!(
            "Failed to canonicalize {description} path from {env_name}: {}",
            path.display()
        )
    })
}
