use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};

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
    let mirrord_binary = resolve_path(
        "MIRRORD_TESTS_USE_BINARY",
        mirrord_binary,
        "external mirrord binary",
    )?;
    let mirrord_layer = resolve_path(
        "MIRRORD_LAYER_FILE",
        mirrord_layer,
        "external mirrord layer",
    )?;

    println!(
        "Running {} tests with mirrord binary {} and layer {}",
        suite.name(),
        mirrord_binary.display(),
        mirrord_layer.display()
    );

    let mut cmd = Command::new("cargo");

    if cargo_nextest_available() {
        cmd.args(["nextest", "run"]);
    } else {
        cmd.arg("test");
    }

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

fn cargo_nextest_available() -> bool {
    Command::new("cargo")
        .args(["nextest", "--version"])
        .status()
        .is_ok_and(|status| status.success())
}

fn resolve_path(env_name: &str, cli_path: Option<PathBuf>, description: &str) -> Result<PathBuf> {
    let path = match cli_path {
        Some(path) => path,
        None => env::var_os(env_name)
            .map(PathBuf::from)
            .with_context(|| format!("{env_name} must point to an existing {description}"))?,
    };

    validate_path(&path, env_name, description)
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
