use std::{env, path::PathBuf, process::Command};

use anyhow::{Context, Result, bail};

/// Runs `cargo doc --document-private-items --no-deps`, providing dummy layer files so the CLI
/// build script doesn't error out. The resulting docs are for reading only — the dummy files mean
/// the built artifacts would be non-functional, but that's irrelevant for documentation.
pub fn run(extra_args: Vec<String>) -> Result<()> {
    let dummy = create_dummy_layer()?;

    let mut cmd = Command::new("cargo");
    cmd.args(["doc", "--document-private-items", "--no-deps"]);
    cmd.args(extra_args);
    cmd.env("MIRRORD_LAYER_FILE", &dummy);

    if env::consts::OS == "macos" {
        cmd.env("MIRRORD_LAYER_FILE_MACOS_ARM64", &dummy);
    }

    let status = cmd.status().context("Failed to run cargo doc")?;
    if !status.success() {
        bail!("cargo doc failed");
    }

    Ok(())
}

fn create_dummy_layer() -> Result<PathBuf> {
    let dir = env::temp_dir().join("mirrord-xtask-doc");
    std::fs::create_dir_all(&dir).context("Failed to create temp dir for dummy layer")?;
    let path = dir.join("dummy_layer");
    std::fs::write(&path, "").context("Failed to write dummy layer file")?;
    Ok(path)
}
