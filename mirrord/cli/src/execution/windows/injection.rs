use std::ffi::OsString;
use std::error::Error;

use dll_syringe::{process::OwnedProcess as InjectorOwnedProcess, Syringe};

use crate::execution::windows::process::SuspendedProcess;

/// Trait extension for Command to support inject_dll
pub trait SuspendedProcessExtInject {
    fn inject_dll(&self, dll_path: String) -> Result<(), Box<dyn Error>>;
}

impl SuspendedProcessExtInject for SuspendedProcess {

    fn inject_dll(&self, dll_path: String) -> Result<(), Box<dyn Error>> {
        let injector_process =
            InjectorOwnedProcess::from_pid(self.process_info.dwProcessId)?;
        let syringe = Syringe::for_process(injector_process);
        let payload_path = OsString::from(dll_path);
        syringe.inject(payload_path)?;
        Ok(())
    }

}
