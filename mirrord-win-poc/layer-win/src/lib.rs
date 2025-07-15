mod hooks;

use std::thread;

use windows::Win32::{Foundation::HINSTANCE, System::SystemServices::DLL_PROCESS_ATTACH};

use crate::hooks::hook;

#[unsafe(no_mangle)]
#[allow(non_snake_case, unused_variables)]
/// # Safety
/// Can be called by loader only. Must not be called manually.
pub unsafe extern "system" fn DllMain(dll_module: HINSTANCE, fdw_reason: u32, _: *mut ()) -> bool {
    if fdw_reason != DLL_PROCESS_ATTACH {
        return true;
    }

    // todo setup tokio
    thread::spawn(move || {
        hook().expect("Failed to install hooks");
    });

    true
}

#[no_mangle]
pub fn install() {
    // install hooks - read/write envvar
    println!("who called install?!");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        assert_eq!(1, 1);
    }
}
