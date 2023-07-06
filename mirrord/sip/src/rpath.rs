use std::{iter, os::unix::process::ExitStatusExt, path::Path, process::Command};

use crate::error::{Result, SipError};

/// Run `install_name_tool` in a child process to add a loader command for each given rpath entry
/// to the binary in `path`.
pub(crate) fn add_rpaths<P: AsRef<Path>>(path: P, rpath_entries: Vec<String>) -> Result<()> {
    if rpath_entries.is_empty() {
        return Ok(());
    }
    let path_str = path.as_ref().to_string_lossy().to_string();

    // args are `-add_rpath RPATH` for each rpath entry, and the binary's path at the end.
    let args = iter::once(String::from("-add_rpath"))
        .chain(
            rpath_entries
                .into_iter()
                .intersperse(String::from("-add_rpath")),
        )
        .chain(iter::once(path_str));

    let output = Command::new("install_name_tool") // most forgettable tool name ever.
        .args(args)
        .output()?;

    if output.status.success() {
        Ok(())
    } else {
        let code = output.status.into_raw();
        Err(SipError::AddingRpathsFailed(
            code,
            String::from_utf8_lossy(&output.stderr).to_string(),
        ))
    }
}
