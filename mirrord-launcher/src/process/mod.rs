//! Simple utilities for interacting with processes.

use std::path::Path;

pub fn process_name_from_path<T: AsRef<Path>>(path: T) -> Option<String> {
    let path = path.as_ref();

    if std::fs::metadata(path).ok()?.is_file() {
        if let Some(file_name) = path.file_name() {
            return Some(file_name.to_str()?.to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests;
