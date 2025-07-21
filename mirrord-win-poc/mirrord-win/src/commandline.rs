mod common;

use std::{env::Args, fmt};

use common::OwnedWSTR;

#[derive(Debug)]
pub struct TargetCommandline {
    pub applicationname: String,
    pub commandline: Vec<String>,
}

#[derive(Debug)]
pub struct CliConfig {
    pub layer_dll_path: String,
    pub target_commandline: TargetCommandline,
}

pub struct CliError {
    err: &'static str,
}
impl fmt::Debug for CliError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{:}.\nUsage: {:}\n",
            self.err,
            CliConfig::CLI_COMMANDLINE
        )
    }
}
impl From<&'static str> for CliError {
    fn from(value: &'static str) -> Self {
        Self { err: value }
    }
}

impl CliConfig {
    pub const CLI_COMMANDLINE: &str =
        "mirrord-win.exe LAYER_DLL_PATH -- TARGET_APP (TARGET_CMD_ARGS...)";

    pub fn from(args: Args) -> Result<CliConfig, CliError> {
        // println!("{:#?}", args);
        let mut args_iter = args.into_iter().peekable();

        // skip argv[0] - process executable name
        let _ = args_iter.next().ok_or(CliError {
            err: "empty commandline",
        })?;

        Ok(CliConfig {
            layer_dll_path: {
                let val = args_iter.next().ok_or("missing LAYER_DLL_PATH")?;
                if !val.ends_with(".dll") {
                    return Err("invalid LAYER_DLL_PATH (must end with .dll)".into());
                }

                // consume required separator but ignore it
                let _ = args_iter
                    .next_if(|val| val.eq("--"))
                    .ok_or("expected '--' separator before target commandline")?;
                val
            },
            target_commandline: TargetCommandline {
                applicationname: args_iter.next().ok_or("missing TARGET_APP")?,
                commandline: args_iter.collect(),
            },
        })
    }
}

impl TargetCommandline {
    pub fn to_wstr_tup(&self) -> (OwnedWSTR, OwnedWSTR) {
        let applicationname = OwnedWSTR::from_string(&self.applicationname);
        let commandline = OwnedWSTR::from_string(&self.commandline.join(" "));
        (applicationname, commandline)
    }
}
