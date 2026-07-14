use std::{
    ffi::{CStr, CString},
    os::unix::ffi::OsStrExt,
    path::Path,
};

use crate::error::{RemoteBootstrapError, Result};

pub(crate) fn load(binary: &Path) -> Result<()> {
    tracing::info!(remote_layer_binary = %binary.display(), "Loading remote layer");

    let binary = CString::new(binary.as_os_str().as_bytes())?;
    let handle = unsafe { libc::dlopen(binary.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
    if handle.is_null() {
        return Err(RemoteBootstrapError::LayerLoad(dlopen_error()));
    }

    Ok(())
}

fn dlopen_error() -> String {
    let error = unsafe { libc::dlerror() };
    if error.is_null() {
        "unknown dlopen error".to_owned()
    } else {
        unsafe { CStr::from_ptr(error).to_string_lossy().into_owned() }
    }
}
