use std::{
    ffi::OsStr,
    os::unix::{ffi::OsStrExt, process::ExitStatusExt},
    path::Path,
    process::Command,
};

use crate::error::{Result, SipError};

/// Sign the binary at the given path using the host's codesign binary.
/// Consider using apple-codesign crate instead some day..
pub(crate) fn sign<P: AsRef<Path>>(path: P) -> Result<()> {
    let output = Command::new("codesign")
        .arg("-s") // sign with identity
        .arg("-") // adhoc identity
        .arg("-f") // force (might have a signature already)
        .arg(path.as_ref())
        .env_remove("DYLD_INSERT_LIBRARIES") // don't load mirrord into the codesign binary
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        let code = output.status.into_raw(); // Returns wait status if there's no exit status.
        let output_stderr = OsStr::from_bytes(&output.stderr).to_os_string();
        Err(SipError::Sign(code, output_stderr))
    }
}
