use std::fs::File;

use windows::Win32::System::Threading as Win32Threading;

pub struct TargetProcess {
    pub process_information: Win32Threading::PROCESS_INFORMATION,
    pub stdout: File,
}
