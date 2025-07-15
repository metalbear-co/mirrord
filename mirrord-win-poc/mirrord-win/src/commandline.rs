mod common;

use std::env::Args;

use common::OwnedWSTR;

#[derive(Debug)]
pub struct TargetCommandline {
    applicationname: String,
    commandline: Vec<String>,
}

#[derive(Debug)]
pub struct CliConfig {
    #[allow(dead_code)]
    layer_dll_path: String,
    pub target_commandline: TargetCommandline,
}

impl CliConfig {
    pub fn from(args: Args) -> CliConfig {
        let mut args_iter = args.into_iter();

        CliConfig {
            layer_dll_path: {
                let val = (&mut args_iter).next().expect("missing LayerDll path");
                println!("{val:#?}");
                assert!(
                    val.ends_with(".dll"),
                    "missing/invalid LayerDll (must end with .dll)"
                );
                // assuming -- exists for now
                (&mut args_iter).next().expect("missing --");
                val
            },
            target_commandline: TargetCommandline {
                applicationname: (&mut args_iter).next().expect("missing application name"),
                commandline: args_iter.collect(),
            },
        }
    }
}

impl TargetCommandline {
    pub fn to_wstr_tup(&self) -> Result<(OwnedWSTR, OwnedWSTR), ()> {
        let applicationname = OwnedWSTR::from_string(&self.applicationname);
        let commandline = OwnedWSTR::from_string(&self.commandline.join(" "));
        Ok((applicationname, commandline))
    }
}
