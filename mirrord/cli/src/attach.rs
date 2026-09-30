use std::os::windows::io::BorrowedHandle;

use mirrord_layer_lib::process::windows::{
    injection::InjectionMethod,
    sync::{InitWaitOutcome, ParentInitEvents},
};
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
/// 4. Invoked `mirrord attach <pid>` (this function). The pid is positional.
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

    // The value parser rejects iat, so only the two attach methods reach here.
    let (process, loader_state, keep_events_alive) = match args.injection_method {
        // Selecting APC attests the IDE's pre-application primary-thread stop. The attach then
        // returns to the debugger so it can release that stop, so a reference in the target
        // keeps the named events alive for the layer.
        InjectionMethod::Apc => (
            OwnedTarget::open_with_main_thread(args.pid),
            LoaderState::EarlyApc,
            true,
        ),
        InjectionMethod::LoadLibrary => (
            OwnedTarget::open_process(args.pid),
            LoaderState::Unknown,
            false,
        ),
        InjectionMethod::Iat => unreachable!("parse_attach rejects iat"),
    };
    let process = process.map_err(|e| CliError::AttachProcessOpenFailed(args.pid, e))?;
    sub_progress.info(&format!("obtained handle to process {}", args.pid));

    // Create the events before injection. The layer opens them by deriving the same
    // names from its own PID, and signals one when initialization is complete.
    let init_events = ParentInitEvents::create(args.pid)
        .map_err(|e| CliError::AttachInjectionFailed(args.pid, e.to_string()))?;

    let remote_events = keep_events_alive
        .then(|| {
            init_events.keep_alive_in_process(unsafe {
                BorrowedHandle::borrow_raw(process.target().process)
            })
        })
        .transpose()
        .map_err(|e| CliError::AttachInjectionFailed(args.pid, e.to_string()))?;
    let target = process.borrowed().with_loader_state(loader_state);
    let result = unsafe { args.injection_method.injector().inject(&target, &lib_path) };
    if let Some(events) = remote_events
        && (result.is_ok() || result.as_ref().is_err_and(|e| e.is_pending()))
    {
        events.retain();
    }
    let injected = result.map_err(|e| CliError::AttachStorkFailed(args.pid, e))?;
    if injected.timing == stork::LoadTiming::OnResume {
        sub_progress.success(Some(
            "layer loading queued; resume the target to initialize",
        ));
        return Ok(());
    }

    sub_progress.info("waiting for layer to signal injection complete");

    // Watching the failure event and the target itself, not only readiness. A layer that gives up
    // inside `DllMain` can say so, and a target that dies is not worth waiting out: either one
    // used to spend the whole timeout and then report it as a timeout, which names the symptom
    // instead of the cause.
    match init_events
        .wait(
            unsafe { BorrowedHandle::borrow_raw(process.target().process) },
            Some(ATTACH_SIGNAL_TIMEOUT_MS),
        )
        .map_err(|e| CliError::AttachInjectionFailed(args.pid, e.to_string()))?
    {
        InitWaitOutcome::Signaled => {
            sub_progress.success(Some(&format!(
                "layer successfully initialized in process {}",
                args.pid
            )));
            Ok(())
        }
        InitWaitOutcome::Failed => Err(CliError::AttachInjectionFailed(
            args.pid,
            "the layer failed to initialize; its own log names the cause".to_owned(),
        )),
        InitWaitOutcome::ProcessExited => Err(CliError::AttachInjectionFailed(
            args.pid,
            "the target exited before the layer reported ready".to_owned(),
        )),
        InitWaitOutcome::TimedOut => Err(CliError::AttachLayerTimeout(args.pid)),
    }
}
