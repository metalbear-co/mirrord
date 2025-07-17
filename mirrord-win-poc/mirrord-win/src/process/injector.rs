use dll_syringe::{process::OwnedProcess as InjectorOwnedProcess, Syringe};

use crate::process::common::TargetProcess;

pub trait Injector {
    fn inject_dll(&self, dll_path: String);
}

impl Injector for TargetProcess {
    fn inject_dll(&self, dll_path: String) {
        let injector_process = InjectorOwnedProcess::from_pid(self.process_information.dwProcessId)
            .expect("Failed to own process for injection");
        let syringe = Syringe::for_process(injector_process);
        let _ = syringe
            .inject(dll_path)
            .expect("Failed to inject layer-win dll");
        // todo!(wait on named mutex?)
    }
}
