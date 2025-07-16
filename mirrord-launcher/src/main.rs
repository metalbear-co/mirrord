mod handle;
mod launcher;
mod process;
mod registry;

use std::io::{ErrorKind, Result};

use crate::{
    launcher::ifeo::{remove_ifeo, set_ifeo},
    process::{Suspended, absolute_path, create_process, resume_thread},
};
use dll_syringe::{Syringe, process::OwnedProcess};

const DLL_PATH: &str = r#"C:\dev\rust\mirrord\target\debug\mirrord_layer_win.dll"#;

const WATCH: &str = "watch";
const FORGET: &str = "forget";

fn help<T: AsRef<str>>(program_name: T) {
    let program_name = program_name.as_ref();

    println!("\n{program_name} - help");
    println!(
        "\t{WATCH} [process.exe] -- begin watching for process creation, capturing execution."
    );
    println!(
        "\t{FORGET} [process.exe] -- remove override for process creation, will stop capturing execution.\n"
    );
    println!("\t[process.exe] [arguments] -- start process with layer loaded.");
}

fn inject_dll(pid: u32) -> Result<()> {
    let process = OwnedProcess::from_pid(pid)?;
    let syringe = Syringe::for_process(process);
    let _ = syringe.inject(DLL_PATH).unwrap();
    Ok(())
}

fn main() -> Result<()> {
    let mut args = std::env::args();

    let program_name = args.next().unwrap();
    let arg = args.next().ok_or_else(|| {
        help(&program_name);
        ErrorKind::InvalidInput
    })?;

    // The current program path should always be resolvable.
    let program_path = absolute_path(&program_name).unwrap();

    if arg == WATCH {
        let program = args.next().ok_or_else(|| {
            help(&program_name);
            ErrorKind::InvalidInput
        })?;

        let installed = set_ifeo(&program, &program_path);
        if !installed {
            println!(
                "[mirrord] failed installing IFEO! please check your arguments and permissions..."
            );
            return Err(ErrorKind::PermissionDenied.into());
        }

        println!(
            "[mirrord] succesfully installed override from \"{program}\" to \"{program_path}\"..."
        );
    } else if arg == FORGET {
        let program = args.next().ok_or_else(|| {
            help(&program_name);
            ErrorKind::InvalidInput
        })?;

        let removed = remove_ifeo(&program);
        if !removed {
            println!(
                "[mirrord] failed removing IFEO! please check your arguments and permissions..."
            );
            return Err(ErrorKind::PermissionDenied.into());
        }

        println!("[mirrord] succesfully removed override for \"{program}\"...");
    } else {
        // Potentially check for environment variable, etc...

        // This argument will be the program path.
        let original_program = arg;

        // Collect the following arguments to be passed to the child process.
        let args: Vec<String> = args.collect();

        // Nasty!
        remove_ifeo(&original_program);
        let info =
            create_process(&original_program, args, Suspended::Yes).ok_or(ErrorKind::InvalidInput);
        set_ifeo(original_program, program_path);

        // Inject DLL.
        let info = info?;
        inject_dll(info.process_id)?;
        resume_thread(info.thread.get());
    }

    Ok(())
}
