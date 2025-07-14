mod commandline;
mod process;

use std::{
    env::{self, Args},
    thread,
    time::Duration,
};

fn main() {
    let args = env::args();
    println!("Raw args {args:?}");

    let commandline = read_target_cmdline(args);
    // execution strategy - direct running
    println!("Running {commandline:?}");

    // CreateProcess(SUSPENDED)
    let proc_info = process::execute(commandline, true).expect("execute commandline");
    println!("Execute Success! {proc_info:#?}");

    // inject LayerDll
    println!("Sleeping 10 secs!");
    thread::sleep(Duration::from_secs(10));

    // ResumeProcess
    process::resume(&proc_info).expect("Failed to resume process");
}

fn read_target_cmdline(args: Args) -> Vec<String> {
    args.into_iter()
        // ignore all arguments up-to (incl.) "--"
        .skip_while(|arg| arg.ne("--"))
        .skip(1)
        .collect()
}
