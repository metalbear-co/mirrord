use std::os::windows::io::BorrowedHandle;

use mirrord_layer_lib::process::windows::{injection::InjectionMethod, sync::LayerInitEvent};
use mirrord_progress::Progress;
use stork::{LoaderState, OwnedTarget};

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

    let process = match args.injection_method {
        InjectionMethod::Apc => OwnedTarget::open_with_main_thread(args.pid),
        InjectionMethod::LoadLibrary => OwnedTarget::open_process(args.pid),
        InjectionMethod::Iat => {
            return Err(CliError::AttachInjectionFailed(
                args.pid,
                "attach does not support iat".to_owned(),
            ));
        }
    }
    .map_err(|e| CliError::AttachProcessOpenFailed(args.pid, e))?;
    sub_progress.info(&format!("obtained handle to process {}", args.pid));

    // Create the event before injection. The layer opens it by deriving the same
    // name from its own PID and signals it when initialization is complete.
    let init_event = LayerInitEvent::for_parent(args.pid)
        .map_err(|e| CliError::AttachInjectionFailed(args.pid, e.to_string()))?;

    // APC attach returns to the debugger so it can release its early stop.
    // A reference in the target keeps the named event alive for the layer worker.
    let remote_event = if args.injection_method == InjectionMethod::Apc {
        Some(
            init_event
                .keep_alive_in_process(unsafe {
                    BorrowedHandle::borrow_raw(process.target().process)
                })
                .map_err(|e| CliError::AttachInjectionFailed(args.pid, e.to_string()))?,
        )
    } else {
        None
    };
    let target =
        process
            .borrowed()
            .with_loader_state(if args.injection_method == InjectionMethod::Apc {
                LoaderState::EarlyApc
            } else {
                LoaderState::Unknown
            });
    // Selecting APC attests the IDE's pre-application primary-thread stop.
    let result = unsafe { args.injection_method.injector().inject(&target, &lib_path) };
    if let Some(event) = remote_event {
        if result.is_ok() || result.as_ref().is_err_and(|e| e.is_pending()) {
            event.retain();
        }
    }
    let injected = result.map_err(|e| CliError::AttachStorkFailed(args.pid, e))?;
    if injected.timing == stork::LoadTiming::OnResume {
        sub_progress.success(Some(
            "layer loading queued; resume the target to initialize",
        ));
        return Ok(());
    }

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
