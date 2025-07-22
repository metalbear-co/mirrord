use std::{
    env::*,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
};

use super::*;

#[test]
fn try_dummy() {
    let mut files = get_files_path();
    assert!(fs::exists(&files).unwrap());

    let mut dummy = files.clone();
    dummy.push("mirrord-target-dummy.exe");
    assert!(fs::exists(&dummy).unwrap());

    let dll = get_dll_path();
    assert!(fs::exists(&dll).unwrap());

    let process_cmd = std::process::Command::new(dummy.as_os_str())
        .stdout(Stdio::piped())
        .spawn();
    assert!(process_cmd.is_ok());

    let process = process_cmd.unwrap();
    let _ = inject_dll(process.id(), dll).unwrap();

    let output = process.wait_with_output();
    assert!(output.is_ok());

    let output = output.unwrap();
    let stdout = output.stdout;

    println!("\n\n{:?}\n\n", stdout);
    assert!(stdout.starts_with(b"read_file_hook: "));
}
