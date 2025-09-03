//! Utility module for files redirection.

use std::{fs::OpenOptions, path::{Path, PathBuf}};

use mirrord_protocol::file::OpenOptionsInternal;
use winapi::um::winnt::{ACCESS_MASK, FILE_APPEND_DATA, FILE_READ_DATA, FILE_WRITE_DATA};

// This prefix is a way to explicitly indicate that we're looking in
// the global namespace for a path.
const GLOBAL_NAMESPACE_PATH: &str = r#"\??\"#;

pub fn remove_root_dir_from_path<T: AsRef<Path>>(path: T) -> Option<String> {
    let mut path = path.as_ref();

    if !path.has_root() {
        return None;
    }

    // Rust doesn't know how to separate the components in this case.
    path = path.strip_prefix(GLOBAL_NAMESPACE_PATH).ok()?;

    // Skip root dir
    let new_path: PathBuf = path.components().into_iter().skip(1).collect();

    // Turn to string, replace Windows slashes to Linux slashes for ease of use.
    Some(new_path.to_str()?.to_string().replace("\\", "/"))
}