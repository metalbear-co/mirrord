mod commandline;
mod process;
use std::{env, time::Duration};

use commandline::{CliConfig, TargetCommandline};
use dll_syringe::{process::OwnedProcess, Syringe};

pub fn read_config() -> CliConfig {
    CliConfig::from(env::args())
}

pub fn run_targetless(commandline: TargetCommandline, layer_dll_path: String) {
    println!("Running headless target with {commandline:#?}");

    // CreateProcess(SUSPENDED)
    let proc_info = process::execute(commandline, true).expect("execute commandline");
    println!("Execute Success! {proc_info:#?}");

    // inject LayerDll
    let process =
        OwnedProcess::from_pid(proc_info.dwProcessId).expect("Failed to own process for injection");
    let syringe = Syringe::for_process(process);
    let _ = syringe
        .inject(layer_dll_path)
        .expect("Failed to inject layer-win dll");
    // our hooking installation should run be running on DllMain
    // todo!(wait on named mutex?)

    // ResumeProcess
    process::resume(&proc_info).expect("Failed to resume process");
    process::wait(proc_info.hProcess, Duration::from_secs(30));
}
