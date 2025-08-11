mod common;

use std::{env::Args, fmt, path::Path};

use common::OwnedWSTR;
use thiserror::Error;

#[derive(Debug)]
pub struct TargetCommandline {
    pub application_name: String,
    pub command_line: Vec<String>,
}

#[derive(Debug)]
pub struct CliConfig {
    pub layer_dll_path: String,
    pub target_commandline: TargetCommandline,
}

#[derive(Error)]
pub enum CliUsageError {
    #[error("empty command_line")]
    EmptyCommandline(),

    #[error("must specify LAYER_DLL_PATH")]
    LayerDllMissing(),

    #[error("cannot find file LAYER_DLL_PATH: {0}")]
    LayerDllNotFound(String),

    #[error("invalid LAYER_DLL_PATH extension (not .dll): {0}")]
    LayerDllInvalidSuffix(String),

    #[error("expected '--' separator before target command_line, found")]
    SeparatorMissing(),

    #[error("missing TARGET_APP")]
    TargetCommandLineMissing(),
}

const CLI_COMMANDLINE: &str = "mirrord-win.exe LAYER_DLL_PATH -- TARGET_APP (TARGET_CMD_ARGS...)";
impl fmt::Debug for CliUsageError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:}.\nUsage: {:}\n", self, CLI_COMMANDLINE)
    }
}

impl TryFrom<Args> for CliConfig {
    type Error = CliUsageError;

    fn try_from(args: Args) -> Result<CliConfig, Self::Error> {
        // skip argv[0] - process executable name
        let mut args_iter = args.into_iter().skip(1).peekable();

        Ok(CliConfig {
            layer_dll_path: {
                let val = match args_iter.next() {
                    Some(val) => {
                        if !val.ends_with(".dll") {
                            return Err(CliUsageError::LayerDllInvalidSuffix(val));
                        }
                        let dll_path = Path::new(&val);
                        if !(dll_path.exists() && dll_path.is_file()) {
                            return Err(CliUsageError::LayerDllNotFound(val));
                        }
                        Ok(val)
                    }
                    None => Err(CliUsageError::LayerDllMissing()),
                }?;

                // consume required separator but ignore it
                let _ = args_iter
                    .next_if(|val| val.eq("--"))
                    .ok_or(CliUsageError::SeparatorMissing())?;
                val
            },
            target_commandline: TargetCommandline {
                application_name: match args_iter.next() {
                    Some(val) => Ok(val),
                    None => Err(CliUsageError::TargetCommandLineMissing()),
                }?,
                command_line: args_iter.collect(),
            },
        })
    }
}

impl TargetCommandline {
    pub fn to_wstr_tup(&self) -> (OwnedWSTR, OwnedWSTR) {
        let application_name = OwnedWSTR::from(&self.application_name);
        let command_line = OwnedWSTR::from(&self.command_line.join(" "));
        (application_name, command_line)
    }
}
