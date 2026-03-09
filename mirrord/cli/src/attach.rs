use std::ffi::OsString;

use dll_syringe::{Syringe, process::OwnedProcess};
use mirrord_layer_lib::process::windows::sync::LayerInitEvent;
use mirrord_progress::Progress;

use crate::{CliResult, config::AttachArgs, error::CliError, extract::extract_library};

const ATTACH_SIGNAL_TIMEOUT_MS: u32 = 30_000;

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

    // Create the attach event before injection. The layer reads this by name (derived from its
    // own PID) and signals it when initialization is complete.
    let init_event = LayerInitEvent::for_attach_parent(args.pid)
        .map_err(|e| CliError::AttachInjectionFailed(args.pid, e.to_string()))?;

    let syringe = Syringe::for_process(process);
    syringe
        .inject(OsString::from(&lib_path))
        .map_err(|e| CliError::AttachInjectionFailed(args.pid, e.to_string()))?;

    sub_progress.info("waiting for layer to signal injection complete");

    match init_event
        .wait_for_signal(Some(ATTACH_SIGNAL_TIMEOUT_MS))
        .map_err(|e| CliError::AttachInjectionFailed(args.pid, e.to_string()))?
    {
        true => {
            sub_progress.success(Some(&format!(
                "layer successfully initialized in process {}",
                args.pid
            )));
            Ok(())
        }
        false => Err(CliError::AttachLayerTimeout(args.pid)),
    }
}
