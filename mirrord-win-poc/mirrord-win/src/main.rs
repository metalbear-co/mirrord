mod commandline;
mod process;

use std::env;

use commandline::CliConfig;
use dll_syringe::{process::OwnedProcess, Syringe};

fn main() {
    let config = CliConfig::from(env::args());
    println!("Raw args {config:?}");

    let commandline = config.target_commandline;
    // execution strategy - direct running
    println!("Running {commandline:#?}");

    // CreateProcess(SUSPENDED)
    let proc_info = process::execute(commandline, true).expect("execute commandline");
    println!("Execute Success! {proc_info:#?}");

    // inject LayerDll
    let process =
        OwnedProcess::from_pid(proc_info.dwProcessId).expect("Failed to own process for injection");
    let syringe = Syringe::for_process(process);
    let _ = syringe
        .inject(config.layer_dll_path)
        .expect("Failed to inject layer-win dll");

    // ResumeProcess
    process::resume(&proc_info).expect("Failed to resume process");
}
