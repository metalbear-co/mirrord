use std::error::Error;

use dll_syringe::{process::OwnedProcess as InjectorOwnedProcess, Syringe};

use crate::process::common::TargetProcess;

pub trait Injector {
    fn inject_dll(&self, dll_path: String) -> Result<(), Box<dyn Error>>;
}

impl Injector for TargetProcess {
    fn inject_dll(&self, dll_path: String) -> Result<(), Box<dyn Error>> {
        let injector_process =
            InjectorOwnedProcess::from_pid(self.process_information.dwProcessId)?;
        let syringe = Syringe::for_process(injector_process);

        let payload_path = if !dll_path.ends_with("\0") {
            dll_path
        } else {
            dll_path + "\0\0" // add wide null termination
        };

        syringe.inject(payload_path)?;
        Ok(())
    }
}
