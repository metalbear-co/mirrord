use std::{fs::File, path::PathBuf};

/// Tests `statx` hooks.
fn main() {
    // Relative path should be handled locally.
    let local_exists = PathBuf::from("./some/local/file").exists();
    assert!(!local_exists);

    // Absolute path should be handled remotely.
    let remote_exists = PathBuf::from("/some/remote/file/1").exists();
    assert!(remote_exists);

    // Absolute path should be opened remotely.
    let file = File::open("/some/remote/file/2").unwrap();
    let _metadata = file.metadata().unwrap();
}
