use std::{
    fs::{self, File},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

const AGENT_BINARY_ENV: &str = "MIRRORD_AGENT_SIDECAR_BINARY";
const DEFAULT_AGENT_BINARY: &str = "/tmp/mirrord/mirrord-agent";

pub(crate) fn extract_agent_binary() -> Result<PathBuf, String> {
    let target_path = std::env::var(AGENT_BINARY_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_AGENT_BINARY));

    write_agent_binary(&target_path)?;

    Ok(target_path)
}

fn write_agent_binary(target_path: &Path) -> Result<(), String> {
    if let Some(agent_dir) = target_path.parent() {
        fs::create_dir_all(agent_dir).map_err(|error| {
            format!("failed creating agent binary parent directory {agent_dir:?}: {error}")
        })?;
    }

    if target_path.exists() {
        tracing::debug!(agent_binary = %target_path.display(), "Reusing existing agent binary");
        return Ok(());
    }

    tracing::info!(agent_binary = %target_path.display(), "Extracting embedded agent binary");

    let mut file = File::create(target_path).map_err(|error| {
        format!("failed creating extracted agent binary at {target_path:?}: {error}")
    })?;
    file.write_all(include_bytes!(env!("MIRRORD_AGENT_BINARY")))
        .map_err(|error| {
            format!("failed writing embedded agent binary to {target_path:?}: {error}")
        })?;

    let mut permissions = file
        .metadata()
        .map_err(|error| {
            format!("failed reading extracted agent binary metadata at {target_path:?}: {error}")
        })?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(target_path, permissions).map_err(|error| {
        format!(
            "failed setting execute permissions on extracted agent binary {target_path:?}: {error}"
        )
    })?;

    tracing::debug!(agent_binary = %target_path.display(), "Finished extracting agent binary");
    Ok(())
}
