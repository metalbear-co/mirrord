mod commandline;
mod process;

use std::{env, time::Duration};

use crate::{
    commandline::{CliConfig, TargetCommandline},
    process::{injector::Injector, TargetProcess},
};

pub fn read_config() -> CliConfig {
    CliConfig::from(env::args())
}

pub fn run_targetless(commandline: TargetCommandline, layer_dll_path: String) {
    println!("Running headless target with {commandline:#?}");

    // CreateProcess(SUSPENDED)
    let mut process = TargetProcess::execute(commandline, true).expect("execute commandline");
    println!("Execute Success!\n{:#?}", process.process_information);

    // inject LayerDll
    process.inject_dll(layer_dll_path);

    // ResumeProcess
    process.resume().expect("Failed to resume process");
    process.join(Duration::from_secs(30));

    println!(
        "Child Process stdout: {:?}",
        process.output() // .expect("failed to read child process stdio")
    );
}

#[cfg(test)]
mod tests {
    // use super::*;

    // let LAYER_DLL_PATH = "C:/Users/Daniel/git/mirrord/target/debug/layer-win.dll"

    #[test]
    fn targetless_works() {}
}
