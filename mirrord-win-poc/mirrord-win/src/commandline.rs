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
    pub layer_dll_path: String,
    pub target_commandline: TargetCommandline,
}

impl CliConfig {
    pub fn from(args: Args) -> CliConfig {
        // println!("{:#?}", args);
        let mut args_iter = args.into_iter();

        // skip argv[0] - process executable name
        let _ = args_iter.next();

        CliConfig {
            layer_dll_path: {
                let val = args_iter.next().expect("missing LayerDll path");
                assert!(
                    val.ends_with(".dll"),
                    "missing/invalid LayerDll (must end with .dll)"
                );
                // assuming -- exists for now
                assert!(args_iter.next().expect("missing --") == "--");
                val
            },
            target_commandline: TargetCommandline {
                applicationname: args_iter.next().expect("missing application name"),
                commandline: args_iter.collect(),
            },
        }
    }
}

impl TargetCommandline {
    pub fn to_wstr_tup(&self) -> (OwnedWSTR, OwnedWSTR) {
        let applicationname = OwnedWSTR::from_string(&self.applicationname);
        let commandline = OwnedWSTR::from_string(&self.commandline.join(" "));
        (applicationname, commandline)
    }
}
