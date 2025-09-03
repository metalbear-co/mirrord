//! Utility module for files redirection.

use std::path::{Path, PathBuf};

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

    let new_path: PathBuf = path.components().into_iter().skip(1).collect();
    Some(new_path.to_str()?.to_string())
}