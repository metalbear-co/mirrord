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

use std::path::Path;

use crate::{process::{absolute_path, process_name_from_path}, registry::Registry};

const IMAGE_FILE_EXECUTION_OPTIONS: &str =
    r#"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options"#;
const DEBUGGER_VALUE: &str = "Debugger";

fn get_ifeo<T: AsRef<str>>(program: T) -> Option<Registry> {
    let hklm = Registry::hklm();
    hklm.get_key(IMAGE_FILE_EXECUTION_OPTIONS)?
        .get_or_insert_key(program)
}

/// Sets IFEO entry for `program`, redirecting it's execution towards `debug`.
/// 
/// # Arguments
/// 
/// * `program` - Path, relative/absolute, towards program to be overriden.
/// * `debug` - Path, relative/absolute, towards program to override.
pub fn set_ifeo<T: AsRef<Path>, U: AsRef<Path>>(program: T, debug: U) -> bool {
    // Truncate any potential path to it's potential file name.
    let program = process_name_from_path(program);
    if program.is_none() {
        return false;
    }

    let program = program.unwrap();

    // Turn path into absolute path to have non-ambiguous override.
    let debug = absolute_path(debug);
    if debug.is_none() {
        return false;
    }

    let debug = debug.unwrap();

    // Remove IFEO before installing.
    remove_ifeo(&program);

    // Install IFEO.
    if let Some(mut ifeo) = get_ifeo(program) {
        let inserted = ifeo.insert_value_string(DEBUGGER_VALUE, debug);
        ifeo.flush();
        return inserted;
    }

    false
}

/// Removes IFEO entry for `program`, re-establishing normal execution.
/// 
/// # Arguments
/// 
/// * `program` - Path, absolute/relative, towards program to remove IFEO for.
pub fn remove_ifeo<T: AsRef<Path>>(program: T) -> bool {
    // Truncate any potential path to it's potential file name.
    let program = process_name_from_path(program);
    if program.is_none() {
        return false;
    }

    let program = program.unwrap();

    if let Some(mut ifeo) = get_ifeo(program) {
        let deleted = ifeo.delete_value(DEBUGGER_VALUE);
        ifeo.flush();
        return deleted;
    }

    false
}
