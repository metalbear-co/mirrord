use std::{error::Error, ffi::OsString};

use dll_syringe::{Syringe, process::OwnedProcess as InjectorOwnedProcess};

use crate::execution::windows::process::SuspendedProcess;

/// Trait extension for Command to support inject_dll
pub trait SuspendedProcessExtInject {
    fn inject_dll(&self, dll_path: String) -> Result<(), Box<dyn Error>>;
}

impl SuspendedProcessExtInject for SuspendedProcess {
    fn inject_dll(&self, dll_path: String) -> Result<(), Box<dyn Error>> {
        let injector_process = InjectorOwnedProcess::from_pid(self.process_info.dwProcessId)?;
        let syringe = Syringe::for_process(injector_process);
        let payload_path = OsString::from(dll_path);
        syringe.inject(payload_path)?;
        Ok(())
    }
}
