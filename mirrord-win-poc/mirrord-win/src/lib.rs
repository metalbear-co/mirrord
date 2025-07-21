mod commandline;
mod process;

use std::{env, error::Error, time::Duration};

use crate::{
    commandline::{CliConfig, TargetCommandline},
    process::{injector::Injector, TargetProcess},
};

pub fn read_config() -> CliConfig {
    CliConfig::from(env::args()).expect("Failed parsing commandline")
}

pub fn run_targetless(
    commandline: TargetCommandline,
    layer_dll_path: String,
) -> Result<String, Box<dyn Error>> {
    println!("Running headless target with {commandline:#?}");

    // CreateProcess(SUSPENDED)
    let mut process = TargetProcess::execute(commandline, true)?;
    println!("Execute Success!\n{:#?}", process.process_information);

    // inject LayerDll
    process.inject_dll(layer_dll_path)?;

    // ResumeProcess
    process.resume()?;
    process.join(Duration::from_secs(30));

    Ok(process.output()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMMY_TARGET: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "\\..\\..\\target\\debug\\target-dummy.exe"
    );
    // layer-win must be in release to prevent Detours prints
    const LAYER_DLL_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "\\..\\..\\target\\release\\layer_win.dll"
    );

    // from layer-win/hooks.rs:try_hijack_env_key
    const ENV_HIJACKED_KEY: &str = "HIJACK_ME";
    const ENV_HIJACKED_VAL: &str = "HIJACKED";

    #[test]
    fn targetless_without_hijack() {
        let val = "VAL";
        let commandline = TargetCommandline {
            applicationname: DUMMY_TARGET.into(),
            commandline: format!("ENVKEY {val}")
                .split_whitespace()
                .map(|s| s.to_string())
                .collect(),
        };
        let layer_dll = String::from(LAYER_DLL_PATH);

        let dummy_target_stdout =
            run_targetless(commandline, layer_dll).expect("Failed to run targetless");

        assert!(
            dummy_target_stdout.trim().eq(val),
            "env:ENVKEY should be set to {val}, got {dummy_target_stdout}"
        )
    }

    #[test]
    fn targetless_hijack_works() {
        let commandline = TargetCommandline {
            applicationname: DUMMY_TARGET.into(),
            commandline: format!("{ENV_HIJACKED_KEY} THIS_IS_GONNA_BE_HIJACKED")
                .split_whitespace()
                .map(|s| s.to_string())
                .collect(),
        };
        let layer_dll = String::from(LAYER_DLL_PATH);

        let dummy_target_stdout =
            run_targetless(commandline, layer_dll).expect("Failed to run targetless");

        assert!(
            dummy_target_stdout.starts_with(ENV_HIJACKED_VAL),
            "env:ENVKEY should be hijacked to {ENV_HIJACKED_VAL}, got {dummy_target_stdout}"
        )
    }
}
