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
    layer_dll_path: String,
    pub target_commandline: TargetCommandline,
}

impl CliConfig {
    pub fn from(args: Args) -> CliConfig {
        let mut args_iter = args.into_iter();

        CliConfig {
            layer_dll_path: {
                let val = (&mut args_iter).next().expect("missing LayerDll path");
                (&mut args_iter).next().expect("missing --"); // assuming -- exists for now
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
        // if commandline.len() < 1 {
        //     return Err(());
        // }

        // let ([applicationname], commandline) = commandline.split_at(1) else {
        //     return Err(());
        // };

        let applicationname = OwnedWSTR::from_string(&self.applicationname);
        let commandline = OwnedWSTR::from_string(&self.commandline.join(" "));
        Ok((applicationname, commandline))
    }
}
