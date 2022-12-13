use mirrord_progress::TaskProgress;

use crate::config::ExtensionExecArgs;

/// Faciliate the execution of a process using mirrord by an IDE extension
pub(crate) fn extension_exec(args: &ExtensionExecArgs) -> Result<()> {
    let progress = TaskProgress::new("Extension Exec");
    Ok(())
}
