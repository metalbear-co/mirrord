//! IFEO module for `launcher`.
//!
//! Responsible for doing operations on the "Image File Execution Options" system
//! in Windows, which is managed through the registry.
//!
//! Among others, this system allows to override process creation through
//! the registry value `Debugger`, which is checked against in `CreateProcessInternalW`.
//!
//! Registering this should allow us to create a global override for our target process,
//! and we have to mitigate the effect of that, to avoid capturing stray processes.

use crate::registry::Registry;

pub const IMAGE_FILE_EXECUTION_OPTIONS: &str =
    r#"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options"#;
pub const DEBUGGER_VALUE: &str = "Debugger";

fn get_ifeo<T: AsRef<str>>(program: T) -> Option<Registry> {
    let hklm = Registry::hklm();
    hklm.get_key(IMAGE_FILE_EXECUTION_OPTIONS)?
        .get_or_insert_key(program)
}

pub fn set_ifeo<T: AsRef<str>, U: AsRef<str>>(program: T, debug: U) -> bool {
    remove_ifeo(&program);

    if let Some(mut ifeo) = get_ifeo(program) {
        let inserted = ifeo.insert_value_string(DEBUGGER_VALUE, debug.as_ref().into());
        ifeo.flush();
        return inserted;
    }

    false
}

pub fn remove_ifeo<T: AsRef<str>>(program: T) -> bool {
    if let Some(mut ifeo) = get_ifeo(program) {
        let deleted = ifeo.delete_value(DEBUGGER_VALUE);
        ifeo.flush();
        return deleted;
    }

    false
}
