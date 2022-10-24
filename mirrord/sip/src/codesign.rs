use std::{path::Path, process::Command};

use crate::error::{Result, SipError};

pub(crate) fn sign<P: AsRef<Path>>(path: P) -> Result<()> {
    let output = Command::new("codesign")
        .arg("-s") // sign with identity
        .arg("-") // adhoc identity
        .arg("-f") // force (might have a signature already)
        .arg(path.as_ref())
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        let code = output.status.code().unwrap(); // shuoldn't happen
        Err(SipError::Sign(
            code,
            String::from_utf8_lossy(&output.stderr).to_string(),
        ))
    }
}
