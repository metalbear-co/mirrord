use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};

use super::layer::{self, Target};

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

/// Resolves the mirrord binary and layer paths.
///
/// Uses CLI args first, then env vars. If neither is set for a given artifact, builds it from
/// source using the current host platform.
fn resolve_artifacts(
    binary_arg: Option<PathBuf>,
    layer_arg: Option<PathBuf>,
) -> Result<(PathBuf, PathBuf)> {
    let binary_from_env = env::var_os("MIRRORD_TESTS_USE_BINARY").map(PathBuf::from);
    let layer_from_env = env::var_os("MIRRORD_LAYER_FILE").map(PathBuf::from);

    let binary = binary_arg.or(binary_from_env);
    let layer = layer_arg.or(layer_from_env);

    match (binary, layer) {
        (Some(b), Some(l)) => {
            let b = validate_path(&b, "MIRRORD_TESTS_USE_BINARY", "mirrord binary")?;
            let l = validate_path(&l, "MIRRORD_LAYER_FILE", "mirrord layer")?;
            Ok((b, l))
        }
        (Some(b), None) => {
            let b = validate_path(&b, "MIRRORD_TESTS_USE_BINARY", "mirrord binary")?;
            let l = build_layer_for_host()?;
            Ok((b, l))
        }
        (None, Some(l)) => {
            let l = validate_path(&l, "MIRRORD_LAYER_FILE", "mirrord layer")?;
            let b = build_binary_for_host(&l)?;
            Ok((b, l))
        }
        (None, None) => {
            println!("No mirrord artifacts provided — building from source...");
            let l = build_layer_for_host()?;
            let b = build_binary_for_host(&l)?;
            Ok((b, l))
        }
    }
}

fn host_target() -> Result<Target> {
    match (env::consts::OS, env::consts::ARCH) {
        ("macos", "x86_64") => Ok(Target::MacosX86_64),
        ("macos", "aarch64") => Ok(Target::MacosAarch64),
        ("linux", "x86_64") => Ok(Target::LinuxX86_64),
        ("linux", "aarch64") => Ok(Target::LinuxAarch64),
        ("windows", _) => Ok(Target::Windows),
        (os, arch) => bail!("Unsupported host platform: {os} {arch}"),
    }
}

fn build_layer_for_host() -> Result<PathBuf> {
    let target = host_target()?;
    layer::build_layer(target, false, &[])
}

fn build_binary_for_host(layer_path: &Path) -> Result<PathBuf> {
    let target = host_target()?;
    super::monitor::build_monitor()?;
    super::cli::build_cli(target, false, layer_path, None, &[])
}

/// Runs CLI unit tests with placeholder embedded assets.
///
/// The unit tests exercise argument parsing and other logic, not the embedded layer or wizard
/// payloads. Supplying small placeholder files keeps the test job independent from frontend and
/// layer build steps while still satisfying the compile-time `include_bytes!` requirements.
pub fn run_unit(cargo_args: Vec<String>) -> Result<()> {
    let assets = create_dummy_cli_artifacts()?;

    let mut cmd = Command::new("cargo");
    cmd.args(["test", "-p", "mirrord", "--features", "wizard"]);
    cmd.args(cargo_args);
    cmd.env("MIRRORD_LAYER_FILE", &assets.layer);
    cmd.env("MIRRORD_LAYER_FILE_MACOS_ARM64", &assets.arm64_layer);
    cmd.env("WIZARD_DIST_DIR", &assets.wizard_dist);
    cmd.env("MIRRORD_SIP_BINARIES_TAR", &assets.sip_binaries_archive);

    let status = cmd.status().context("Failed to run cargo test command")?;
    if !status.success() {
        bail!("unit tests failed");
    }

    Ok(())
}

fn cargo_nextest_available() -> bool {
    Command::new("cargo")
        .args(["nextest", "--version"])
        .status()
        .is_ok_and(|status| status.success())
}

struct DummyCliArtifacts {
    layer: PathBuf,
    arm64_layer: PathBuf,
    wizard_dist: PathBuf,
    sip_binaries_archive: PathBuf,
}

fn create_dummy_cli_artifacts() -> Result<DummyCliArtifacts> {
    let dir = env::temp_dir().join("mirrord-xtask-unit");
    std::fs::create_dir_all(&dir).context("Failed to create temp dir for dummy CLI assets")?;

    let layer = create_dummy_file(&dir.join("libmirrord_layer.so"))?;
    let arm64_layer = create_dummy_file(&dir.join("libmirrord_layer.dylib"))?;
    let wizard_dist = create_dummy_wizard_dist(&dir.join("wizard-dist"))?;
    let sip_binaries_archive = create_dummy_file(&dir.join("sip-binaries.tar.gz"))?;

    Ok(DummyCliArtifacts {
        layer,
        arm64_layer,
        wizard_dist,
        sip_binaries_archive,
    })
}

fn create_dummy_file(path: &Path) -> Result<PathBuf> {
    std::fs::write(path, []).with_context(|| format!("Failed to write {}", path.display()))?;
    path.canonicalize()
        .with_context(|| format!("Failed to canonicalize {}", path.display()))
}

fn create_dummy_wizard_dist(path: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(path)
        .with_context(|| format!("Failed to create {}", path.display()))?;
    std::fs::write(path.join("index.html"), [])
        .with_context(|| format!("Failed to write {}", path.join("index.html").display()))?;
    path.canonicalize()
        .with_context(|| format!("Failed to canonicalize {}", path.display()))
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
