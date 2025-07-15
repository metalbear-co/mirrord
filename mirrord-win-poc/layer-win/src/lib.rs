mod hooks;

use crate::hooks::hook;

// #[unsafe(no_mangle)]
// #[allow(non_snake_case, unused_variables)]
// /// # Safety
// /// Can be called by loader only. Must not be called manually.
// pub unsafe extern "system" fn DllMain(dll_module: HINSTANCE, fdw_reason: u32, _: *mut ()) -> bool
// {}

#[no_mangle]
pub fn install() {
    // install hooks - read/write envvar
    println!("install: start");
    hook().expect("Failed to install hooks");
    println!("install done");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        assert_eq!(1, 1);
    }
}
