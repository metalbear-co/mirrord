use std::ffi::OsString;

use dll_syringe::{Syringe, process::OwnedProcess};
use mirrord_layer_lib::process::windows::sync::LayerInitEvent;
use mirrord_progress::Progress;

use crate::{CliResult, config::AttachArgs, error::CliError, extract::extract_library};

const ATTACH_SIGNAL_TIMEOUT_MS: u32 = 30_000;

/// Attach the mirrord layer to an already-running process by injecting the layer DLL.
///
/// This is only the DLL-injection half of the attach flow. By the time this function runs,
/// the **IDE extension** (mirrord-vscode) has already done all the heavy lifting:
///
/// 1. Started the intproxy.
/// 2. Retrieved the necessary environment variables from the agent/intproxy (proxy address, layer
///    ID, resolved config, etc.).
/// 3. Injected those environment variables into the target process (through editing the debug
///    launch configuration).
/// 4. Invoked `mirrord attach --pid <pid>` (this function).
///
/// Because of this, `attach_command` does **not** spawn an intproxy, resolve a k8s
/// target, or set up any environment variables itself — all of that state already
/// exists in the target process's environment before we get here.
///
/// Presently, the debugging VSCode instance is waiting for attach to finish.
///
/// When we are done, the VSCode instance will do post-attach chores, such as
/// resuming the user-code process' primary execution thread, making this
/// whole process invisible to the user.
///
/// The extension-side implementation was introduced in the following pull request:
/// <https://github.com/metalbear-co/mirrord-vscode/pull/210>.
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

    // Create the event before injection. The layer opens it by deriving the same
    // name from its own PID and signals it when initialization is complete.
    let init_event = LayerInitEvent::for_parent(args.pid)
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
