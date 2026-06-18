use std::{
    fs::{self, File},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

const AGENT_BINARY_ENV: &str = "MIRRORD_AGENT_SIDECAR_BINARY";
const INTPROXY_REMOTE_BINARY_ENV: &str = "MIRRORD_INTPROXY_REMOTE_BINARY";
const DEFAULT_AGENT_BINARY: &str = "/tmp/mirrord/mirrord-agent";
const DEFAULT_INTPROXY_REMOTE_BINARY: &str = "/tmp/mirrord/mirrord-intproxy-remote";

pub(crate) fn extract_agent_binary() -> Result<PathBuf, String> {
    let target_path = std::env::var(AGENT_BINARY_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_AGENT_BINARY));

    write_binary(
        &target_path,
        include_bytes!(env!("MIRRORD_AGENT_BINARY")),
        "agent",
    )?;

    Ok(target_path)
}

pub(crate) fn extract_intproxy_remote_binary() -> Result<PathBuf, String> {
    let target_path = std::env::var(INTPROXY_REMOTE_BINARY_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_INTPROXY_REMOTE_BINARY));

    write_binary(
        &target_path,
        include_bytes!(env!("MIRRORD_INTPROXY_REMOTE_BINARY")),
        "remote intproxy",
    )?;

    Ok(target_path)
}

fn write_binary(target_path: &Path, bytes: &[u8], binary_name: &str) -> Result<(), String> {
    if let Some(agent_dir) = target_path.parent() {
        fs::create_dir_all(agent_dir).map_err(|error| {
            format!("failed creating {binary_name} binary parent directory {agent_dir:?}: {error}")
        })?;
    }

    if target_path.exists() {
        tracing::debug!(binary = %target_path.display(), %binary_name, "Reusing existing embedded binary");
        return Ok(());
    }

    tracing::info!(binary = %target_path.display(), %binary_name, "Extracting embedded binary");

    let mut file = File::create(target_path).map_err(|error| {
        format!("failed creating extracted {binary_name} binary at {target_path:?}: {error}")
    })?;
    file.write_all(bytes).map_err(|error| {
        format!("failed writing embedded {binary_name} binary to {target_path:?}: {error}")
    })?;

    let mut permissions = file
        .metadata()
        .map_err(|error| {
            format!(
                "failed reading extracted {binary_name} binary metadata at {target_path:?}: {error}"
            )
        })?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(target_path, permissions).map_err(|error| {
        format!(
            "failed setting execute permissions on extracted {binary_name} binary {target_path:?}: {error}"
        )
    })?;

    tracing::debug!(binary = %target_path.display(), %binary_name, "Finished extracting embedded binary");
    Ok(())
}
