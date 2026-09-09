use std::{
    ffi::{CStr, CString, OsStr, OsString},
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::Path,
};

use crate::error::{RemoteBootstrapError, Result};

const LD_PRELOAD_ENV: &str = "LD_PRELOAD";

/// Loads remote-layer into the application and configures it for inherited child environments.
pub(crate) fn load(binary: &Path) -> Result<()> {
    configure_preload(binary)?;

    tracing::info!(remote_layer_binary = %binary.display(), "Loading remote layer");
    let binary_cstring = CString::new(binary.as_os_str().as_bytes())?;
    let handle =
        unsafe { libc::dlopen(binary_cstring.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
    if handle.is_null() {
        return Err(RemoteBootstrapError::LayerLoad(dlopen_error()));
    }

    Ok(())
}

/// Adds remote-layer to the inherited preload list without reintroducing the bootstrap.
///
/// This runs from the bootstrap constructor, before application threads are started. Keeping the
/// mutation here lets ordinary child-process environment inheritance propagate remote-layer
/// without interposing on process-execution functions.
fn configure_preload(binary: &Path) -> Result<()> {
    let remote_layer = binary.as_os_str().as_bytes();
    if remote_layer
        .iter()
        .any(|byte| *byte == b':' || byte.is_ascii_whitespace())
    {
        return Err(RemoteBootstrapError::InvalidPreloadPath(binary.to_owned()));
    }

    let current = std::env::var_os(LD_PRELOAD_ENV).unwrap_or_default();
    let Some(preload) = merge_preload(current.as_os_str().as_bytes(), remote_layer) else {
        return Ok(());
    };

    tracing::debug!(preload = ?OsStr::from_bytes(&preload), "Configured LD_PRELOAD");
    // SAFETY: The bootstrap constructor performs this before application threads are started.
    unsafe { std::env::set_var(LD_PRELOAD_ENV, OsString::from_vec(preload)) };
    Ok(())
}

fn merge_preload(current: &[u8], remote_layer: &[u8]) -> Option<Vec<u8>> {
    if current
        .split(|byte| *byte == b':' || byte.is_ascii_whitespace())
        .any(|entry| entry == remote_layer)
    {
        return None;
    }

    let mut preload =
        Vec::with_capacity(current.len() + usize::from(!current.is_empty()) + remote_layer.len());
    preload.extend_from_slice(current);
    if !preload.is_empty() {
        preload.push(b':');
    }
    preload.extend_from_slice(remote_layer);
    Some(preload)
}

fn dlopen_error() -> String {
    let error = unsafe { libc::dlerror() };
    if error.is_null() {
        "unknown dlopen error".to_owned()
    } else {
        unsafe { CStr::from_ptr(error).to_string_lossy().into_owned() }
    }
}
