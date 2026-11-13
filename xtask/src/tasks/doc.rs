use anyhow::{Context, Result, bail};
use mirrord_command::resolve_command;

/// Runs `cargo doc --document-private-items --no-deps`. The CLI's embedded files are the
/// placeholders from `.cargo/config.toml`, which don't matter for documentation.
pub fn run(extra_args: Vec<String>) -> Result<()> {
    let mut cmd = resolve_command("cargo");
    cmd.args(["doc", "--document-private-items", "--no-deps"]);
    cmd.args(extra_args);

    let status = cmd.status().context("Failed to run cargo doc")?;
    if !status.success() {
        bail!("cargo doc failed");
    }

    Ok(())
}
