//! Runs with a custom `PATHEXT`, which needs its own test binary: `which` reads `PATHEXT` once per
//! process, so the unit tests can only exercise whatever the machine has set.
#![cfg(windows)]

use std::{env, fs};

use mirrord_command::resolve_command;

#[test]
fn ps1_listed_before_cmd_still_resolves_cmd() {
    let dir = tempfile::tempdir().unwrap();
    for file in ["npm.ps1", "npm.cmd"] {
        fs::write(dir.path().join(file), "").unwrap();
    }

    // SAFETY: this is the only test in this binary, so nothing else reads the environment
    // concurrently.
    unsafe {
        env::set_var("PATHEXT", ".PS1;.CMD");
        env::set_var("PATH", dir.path());
    }

    assert_eq!(
        resolve_command("npm").get_program(),
        dir.path().join("npm.cmd")
    );
}
