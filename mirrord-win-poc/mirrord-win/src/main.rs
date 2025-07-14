mod commandline;
mod process;

use std::env;

use commandline::CliConfig;

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

    // ResumeProcess
    process::resume(&proc_info).expect("Failed to resume process");
}
