use std::{
    fs::{self, File},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use crate::error::Result;

const AGENT_BINARY_ENV: &str = "MIRRORD_REMOTE_AGENT_SIDECAR_BINARY";
const DEFAULT_AGENT_BINARY: &str = "/tmp/mirrord/mirrord-agent";

pub(crate) fn extract_agent_binary() -> Result<PathBuf> {
    extract_binary(
        AGENT_BINARY_ENV,
        DEFAULT_AGENT_BINARY,
        include_bytes!(env!("MIRRORD_AGENT_BINARY")),
        "agent",
    )
}

fn extract_binary(
    target_env: &str,
    default_path: &str,
    bytes: &[u8],
    binary_name: &str,
) -> Result<PathBuf> {
    let target_path = std::env::var(target_env)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(default_path));

    write_binary(&target_path, bytes, binary_name)?;

    Ok(target_path)
}

fn write_binary(target_path: &Path, bytes: &[u8], binary_name: &str) -> Result<()> {
    if target_path.exists() {
        tracing::debug!(binary = %target_path.display(), %binary_name, "Reusing existing binary");
        return Ok(());
    }

    if let Some(parent_dir) = target_path.parent() {
        fs::create_dir_all(parent_dir)?;
    }
    tracing::info!(binary = %target_path.display(), %binary_name, "Extracting embedded binary");

    let mut file = File::create(target_path)?;
    file.write_all(bytes)?;

    let mut permissions = file.metadata()?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(target_path, permissions)?;

    tracing::debug!(binary = %target_path.display(), %binary_name, "Finished extracting embedded binary");
    Ok(())
}
