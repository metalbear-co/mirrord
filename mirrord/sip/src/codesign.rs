use std::{os::unix::process::ExitStatusExt, path::Path, process::Command};

use crate::error::{Result, SipError};

pub(crate) fn sign<P: AsRef<Path>>(path: P) -> Result<()> {
    // don't load mirrord into the codesign binary
    let output = Command::new("codesign")
        .arg("-s") // sign with identity
        .arg("-") // adhoc identity
        .arg("-f") // force (might have a signature already)
        .arg(path.as_ref())
        .env_remove("DYLD_INSERT_LIBRARIES")
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        let code = output.status.into_raw(); // Returns wait status if there's no exit status.
        Err(SipError::Sign(
            code,
            String::from_utf8_lossy(&output.stderr).to_string(),
        ))
    }
}
