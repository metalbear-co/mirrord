use std::error::Error;

use windows::Win32::System::Threading as Win32;

use crate::commandline;

pub fn execute(
    commandline: Vec<String>,
    suspended: bool,
) -> Result<Win32::PROCESS_INFORMATION, Box<dyn Error>> {
    let (appname, mut cmdline) =
        commandline::build(commandline).expect("commandline should be parsable");

    let startup_info = Win32::STARTUPINFOW::default();
    let creation_flags = {
        let mut cf = Win32::PROCESS_CREATION_FLAGS::default();
        if suspended {
            cf |= Win32::CREATE_SUSPENDED;
        };
        cf
    };
    let mut process_information = Win32::PROCESS_INFORMATION::default();

    unsafe {
        Win32::CreateProcessW(
            appname.pcwstr(),
            Some(cmdline.pwstr()),
            None,
            None,
            false,
            creation_flags,
            None,
            None,
            &startup_info,
            &mut process_information,
        )?;
    };
    Ok(process_information)
}
