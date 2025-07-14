use std::error::Error;

use windows::{core as windows_core, Win32::System::Threading as Win32};

use crate::commandline::TargetCommandline;

pub fn execute(
    target_commandline: TargetCommandline,
    suspended: bool,
) -> Result<Win32::PROCESS_INFORMATION, Box<dyn Error>> {
    let (appname, mut cmdline) = target_commandline
        .to_wstr_tup()
        .expect("commandline should be parsable");

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

pub fn resume(process_info: &Win32::PROCESS_INFORMATION) -> windows_core::Result<()> {
    unsafe {
        // ResumeThread: If the function fails, the return value is (DWORD) -1 (u32::MAX)
        if Win32::ResumeThread(process_info.hThread) == u32::MAX {
            return Err(windows_core::Error::from_win32());
        }
    }
    Ok(())
}
