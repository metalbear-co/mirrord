use std::ffi::OsString;

use dll_syringe::{Syringe, process::OwnedProcess as InjectorOwnedProcess};

use crate::{
    error::{ProcessExecError, ProcessExecResult},
    execution::windows::process::{WindowsProcess, WindowsProcessExtSuspended},
};

/// Trait extension for Command to support inject_dll
pub trait WindowsProcessSuspendedExtInject {
    fn inject_dll(&self, dll_path: String) -> ProcessExecResult<()>;
}

impl WindowsProcessSuspendedExtInject for WindowsProcess
where
    WindowsProcess: WindowsProcessExtSuspended,
{
    fn inject_dll(&self, dll_path: String) -> ProcessExecResult<()> {
        let injector_process = InjectorOwnedProcess::from_pid(self.process_info.dwProcessId)
            .map_err(|e| {
                ProcessExecError::ProcessNotFound(self.process_info.dwProcessId, e.to_string())
            })?;
        let syringe = Syringe::for_process(injector_process);
        let payload_path = OsString::from(dll_path.clone());
        syringe.inject(payload_path).map_err(|e| {
            ProcessExecError::InjectionFailed(
                dll_path,
                self.process_info.dwProcessId,
                e.to_string(),
            )
        })?;
        Ok(())
    }
}
