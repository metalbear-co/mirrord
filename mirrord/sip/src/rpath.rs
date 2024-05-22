use std::{
    ffi::{OsStr, OsString},
    iter,
    os::unix::{ffi::OsStrExt, process::ExitStatusExt},
    path::Path,
    process::Command,
};

use crate::error::{Result, SipError};

/// Run `install_name_tool` in a child process to add a loader command for each given rpath entry
/// to the binary in `path`.
pub(crate) fn add_rpaths<P: AsRef<Path>>(path: P, rpath_entries: Vec<OsString>) -> Result<()>
where
    OsString: From<P>,
{
    if rpath_entries.is_empty() {
        return Ok(());
    }

    // args are `-add_rpath RPATH` for each rpath entry, and the binary's path at the end.
    let args = iter::once("-add_rpath".into())
        .chain(rpath_entries.into_iter().intersperse("-add_rpath".into()))
        .chain(iter::once(path.into()));

    let output = Command::new("install_name_tool") // most forgettable tool name ever.
        .args(args)
        .output()?;

    if output.status.success() {
        Ok(())
    } else {
        let code = output.status.into_raw();
        let output_stderr = OsStr::from_bytes(&output.stderr).to_os_string();
        Err(SipError::AddingRpathsFailed(code, output_stderr))
    }
}
