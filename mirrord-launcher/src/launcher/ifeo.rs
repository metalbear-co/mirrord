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

use crate::{
    process::{CreateProcessHandles, Suspended, create_process, process_name_from_path},
    registry::Registry,
};

pub const IMAGE_FILE_EXECUTION_OPTIONS: &str =
    r#"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options"#;
pub const DEBUGGER_VALUE: &str = "Debugger";

fn get_ifeo<T: AsRef<str>>(program: T) -> Option<Registry> {
    let hklm = Registry::hklm();
    hklm.get_key(IMAGE_FILE_EXECUTION_OPTIONS)?
        .get_or_insert_key(program)
}

fn set_ifeo<T: AsRef<str>, U: AsRef<Path>>(program: T, debug: U) -> bool {
    let debug = debug.as_ref();

    remove_ifeo(&program);

    let debug = debug.to_str();
    if debug.is_none() {
        return false;
    }

    if let Some(mut ifeo) = get_ifeo(program) {
        let inserted = ifeo.insert_value_string(DEBUGGER_VALUE, debug.unwrap().into());
        ifeo.flush();
        return inserted;
    }

    false
}

fn remove_ifeo<T: AsRef<str>>(program: T) -> bool {
    if let Some(mut ifeo) = get_ifeo(program) {
        let deleted = ifeo.delete_value(DEBUGGER_VALUE);
        ifeo.flush();
        return deleted;
    }

    false
}

/// Starts program in the context of IFEO "guard", registering IFEO override
/// before the process creation, and removing it later.
///
/// Program may be started suspended depending on arguments.
///
/// # Arguments
///
/// * `program` - Path to program to be started, relative/absolute.
/// * `debug` - Path to program to override execution, relatiev/absolute.
/// * `args` - Arguments to be passed to process creation.
/// * `suspended` - Whether the process should begin in a suspended state or not. Requires resuming.
pub fn start_ifeo<T: AsRef<Path>, U: AsRef<Path>, V: AsRef<[String]>>(
    program: T,
    debug: U,
    args: V,
    suspended: Suspended,
) -> Option<CreateProcessHandles> {
    let program = program.as_ref();

    let file_name = process_name_from_path(program)?;

    let install = set_ifeo(&file_name, debug);
    if !install {
        return None;
    }

    let handles = create_process(program, args, suspended)?;

    let uninstall = remove_ifeo(file_name);
    if uninstall {
        return None;
    }

    Some(handles)
}
