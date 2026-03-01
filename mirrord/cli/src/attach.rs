use std::ffi::OsString;

use dll_syringe::{Syringe, process::OwnedProcess};
use mirrord_progress::Progress;

use crate::{CliResult, config::AttachArgs, error::CliError, extract::extract_library};

/// Attach the mirrord layer to an already-running process by injecting the layer DLL.
///
/// The target process is expected to already have all mirrord environment variables
/// configured (by the IDE extension), so no k8s setup or proxy spawning is needed.
pub(crate) fn attach_command<P>(args: AttachArgs, progress: &P) -> CliResult<()>
where
    P: Progress,
{
    let mut sub_progress = progress.subtask("attaching to process");

    let lib_path = extract_library(None, progress, true)?;

    unsafe { std::env::set_var("MIRRORD_LAYER_FILE", &lib_path) };

    let process = OwnedProcess::from_pid(args.pid)
        .map_err(|e| CliError::AttachProcessOpenFailed(args.pid, e))?;
    sub_progress.info(&format!("obtained handle to process {}", args.pid));

    let syringe = Syringe::for_process(process);
    syringe
        .inject(OsString::from(&lib_path))
        .map_err(|e| CliError::AttachInjectionFailed(args.pid, e.to_string()))?;

    sub_progress.success(Some(&format!("layer injected into process {}", args.pid)));
    Ok(())
}
