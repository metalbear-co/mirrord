mod commandline;
mod process;

use std::{
    env::{self, Args},
    // ffi::os_str::OsString,
};

fn main() {
    let args = env::args();
    let commandline = read_target_cmdline(args);
    // execution strategy - direct running
    let proc_info = process::execute(commandline, true).expect("execute commandline");

    // CreateProcess(SUSPENDED)
    // inject LayerDll
    // ResumeProcess
}

fn read_target_cmdline(args: Args) -> Vec<String> {
    args.into_iter()
        // ignore all arguments up-to (incl.) "--"
        .skip_while(|arg| arg.ne("--"))
        .skip(1)
        .collect()
}
