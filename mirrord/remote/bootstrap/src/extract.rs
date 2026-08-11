//! Extracts the embded artifcated in the bootstrap library.
//!
//! Artifacts default to the directory containing the loaded bootstrap shared object, keeping the
//! packaged files together without assuming that a particular temporary directory exists. The
//! destination can be overridden through environment variables.

use std::{
    ffi::{CStr, OsStr},
    fs::{self, File, Permissions},
    io::{self, Read, Write},
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Path, PathBuf},
};

use tempfile::NamedTempFile;

use crate::error::{RemoteBootstrapError, Result};

const AGENT_BINARY_ENV: &str = "MIRRORD_REMOTE_AGENT_BINARY";
const DEFAULT_AGENT_BINARY_NAME: &str = "mirrord-agent";
const REMOTE_LAYER_BINARY_ENV: &str = "MIRRORD_REMOTE_LAYER_BINARY";
const DEFAULT_REMOTE_LAYER_BINARY_NAME: &str = "libmirrord_remote_layer.so";

pub(crate) fn extract_agent_binary() -> Result<PathBuf> {
    extract_binary(
        AGENT_BINARY_ENV,
        DEFAULT_AGENT_BINARY_NAME,
        include_bytes!(env!("MIRRORD_AGENT_BINARY")),
        "agent",
    )
}

pub(crate) fn extract_remote_layer_binary() -> Result<PathBuf> {
    extract_binary(
        REMOTE_LAYER_BINARY_ENV,
        DEFAULT_REMOTE_LAYER_BINARY_NAME,
        include_bytes!(env!("MIRRORD_REMOTE_LAYER_BINARY")),
        "remote layer",
    )
}

fn extract_binary(
    target_env: &str,
    default_name: &str,
    embedded: &[u8],
    binary_name: &str,
) -> Result<PathBuf> {
    let target_path = match std::env::var_os(target_env) {
        Some(path) => PathBuf::from(path),
        None => default_path(default_name)?,
    };

    if target_path.exists() {
        match matches_embedded(&target_path, embedded) {
            Ok(true) => {
                tracing::info!(
                    binary = %target_path.display(),
                    %binary_name,
                    "Reusing matching binary"
                );
                return Ok(target_path);
            }
            Ok(false) => {
                tracing::warn!(
                    binary = %target_path.display(),
                    %binary_name,
                    "Existing binary mismatches embedded one, overwriting"
                );
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    binary = %target_path.display(),
                    %binary_name,
                    "Failed to examine existing binary, overwriting"
                );
            }
        }
    }

    tracing::info!(
        binary = %target_path.display(),
        %binary_name,
        "Extracting embedded binary"
    );
    write_binary_atomically(&target_path, embedded)?;

    Ok(target_path)
}

fn write_binary_atomically(target_path: &Path, embedded: &[u8]) -> io::Result<()> {
    let target_dir = target_path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(target_dir)?;

    let mut temporary_file = NamedTempFile::new_in(target_dir)?;
    temporary_file.write_all(embedded)?;
    temporary_file
        .as_file()
        .set_permissions(Permissions::from_mode(0o755))?;
    temporary_file
        .persist(target_path)
        .map(|_| ())
        .map_err(|error| error.error)
}

fn default_path(file_name: &str) -> Result<PathBuf> {
    Ok(bootstrap_directory()?.join(file_name))
}

/// Resolves the directory containing this loaded bootstrap shared object.
fn bootstrap_directory() -> Result<PathBuf> {
    let mut info = std::mem::MaybeUninit::<libc::Dl_info>::zeroed();
    let symbol = extract_binary as *const () as *const libc::c_void;
    let result = unsafe { libc::dladdr(symbol, info.as_mut_ptr()) };
    if result == 0 {
        return Err(RemoteBootstrapError::DlAddr("dladdr error".to_owned()));
    }

    let info = unsafe { info.assume_init() };
    if info.dli_fname.is_null() {
        return Err(RemoteBootstrapError::DlAddr("path was empty".to_owned()));
    }

    let shared_object_path = unsafe { CStr::from_ptr(info.dli_fname) };
    Path::new(OsStr::from_bytes(shared_object_path.to_bytes()))
        .parent()
        .map(Path::to_owned)
        .ok_or(RemoteBootstrapError::DlAddr(
            "path had no parent".to_owned(),
        ))
}

/// Returns whether `path` exactly matches the embedded artifact without reading it all at once.
fn matches_embedded(path: &Path, embedded: &[u8]) -> io::Result<bool> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };

    if file.metadata()?.len() != embedded.len() as u64 {
        return Ok(false);
    }

    let mut offset = 0;
    let mut buffer = [0_u8; 64 * 1024];
    while offset < embedded.len() {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            return Ok(false);
        }

        if let Some(end) = offset.checked_add(count)
            && let Some(buffer_chunk) = buffer.get(..count)
            && let Some(embedded_chunk) = embedded.get(offset..end)
            && buffer_chunk == embedded_chunk
        {
            offset = end;
        } else {
            return Ok(false);
        }
    }

    Ok(true)
}
