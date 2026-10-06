//! Builds [`Command`]s for external programs in a way that also finds them on Windows.
//!
//! [`Command::new`] on Windows only ever appends `.exe` to a bare program name, so tools that are
//! installed as `.cmd`/`.bat` shims (`npm`, `pnpm`, `corepack`, ...) are never found, even though
//! `cmd.exe` and PowerShell run them just fine. [`resolve_command`] looks bare names up in `PATH`
//! with `PATHEXT` first, the same way a shell does. Everywhere else it is just [`Command::new`].
//!
//! The workspace `clippy.toml` bans [`Command::new`] (and its tokio counterpart) in favor of the
//! helpers here, so new call sites can't reintroduce `if cfg!(windows) { "npm.cmd" }` workarounds.

use std::{ffi::OsStr, process::Command};

/// Drop-in replacement for [`Command::new`] that also resolves `.cmd`/`.bat` shims on Windows.
///
/// On Windows, a bare program name (no path separators) is searched for in each `PATH` entry in
/// order, trying the extensions from `PATHEXT` in order within each entry. Anything else (absolute
/// or relative paths) is passed through untouched. If the lookup finds nothing, this falls back to
/// [`Command::new`], so spawning fails with the usual [`std::io::ErrorKind::NotFound`].
///
/// The lookup uses this process's `PATH`, not one set later on the returned [`Command`].
///
/// Only matches that [`Command`] can spawn are used: `.exe` and `.com` files, and `.bat`/`.cmd`
/// scripts, which it runs through `cmd.exe`. Other `PATHEXT` entries (`.js`, `.vbs`, or a
/// user-added `.ps1`) need a script host, so spawning them fails with "not a valid Win32
/// application". They are skipped, which keeps `npm` resolving to `npm.cmd` even when `PATHEXT`
/// lists `.PS1` first.
#[allow(clippy::disallowed_methods, reason = "this is the sanctioned wrapper")]
pub fn resolve_command<S: AsRef<OsStr>>(program: S) -> Command {
    #[cfg(windows)]
    if let Some(resolved) = windows::resolve(program.as_ref(), std::env::var_os("PATH")) {
        return Command::new(resolved);
    }

    Command::new(program)
}

/// [`resolve_command`] for [`tokio::process::Command`].
#[cfg(feature = "tokio")]
pub fn resolve_tokio_command<S: AsRef<OsStr>>(program: S) -> tokio::process::Command {
    resolve_command(program).into()
}

#[cfg(windows)]
mod windows {
    use std::{
        ffi::{OsStr, OsString},
        path::{Component, Path, PathBuf},
    };

    /// Finds `program` in `path` (a `PATH`-style list), honoring `PATHEXT`.
    ///
    /// Returns [`None`] for anything that is not a bare program name, so explicit paths keep
    /// [`std::process::Command`]'s own handling.
    ///
    /// `which` yields every match in `PATH`-then-`PATHEXT` order, including extensionless binaries
    /// and scripts `Command` can't spawn, so this takes the first match it can spawn.
    pub(super) fn resolve(program: &OsStr, path: Option<OsString>) -> Option<PathBuf> {
        let mut components = Path::new(program).components();
        let is_bare_name = matches!(
            (components.next(), components.next()),
            (Some(Component::Normal(_)), None)
        );
        if !is_bare_name {
            return None;
        }

        which::which_in_global(program, path).ok()?.find(|found| {
            found.extension().is_some_and(|extension| {
                ["exe", "com", "bat", "cmd"]
                    .iter()
                    .any(|spawnable| extension.eq_ignore_ascii_case(spawnable))
            })
        })
    }

    #[cfg(test)]
    mod tests {
        use std::{env, fs, path::Path};

        use tempfile::TempDir;

        use super::*;
        use crate::resolve_command;

        fn dir_with(files: &[&str]) -> TempDir {
            let dir = tempfile::tempdir().unwrap();
            for file in files {
                fs::write(
                    dir.path().join(file),
                    "@echo off\r\necho resolved %~nx0\r\n",
                )
                .unwrap();
            }
            dir
        }

        fn path_of(dirs: &[&Path]) -> Option<OsString> {
            Some(env::join_paths(dirs).unwrap())
        }

        #[test]
        #[allow(
            clippy::disallowed_methods,
            reason = "demonstrates why the raw constructor is banned"
        )]
        fn finds_and_runs_cmd_shim() {
            let dir = dir_with(&["mirrord-shim.cmd"]);

            let not_found = std::process::Command::new("mirrord-shim")
                .env("PATH", dir.path())
                .output()
                .unwrap_err();
            assert_eq!(not_found.kind(), std::io::ErrorKind::NotFound);

            let resolved = resolve(OsStr::new("mirrord-shim"), path_of(&[dir.path()])).unwrap();
            assert_eq!(resolved, dir.path().join("mirrord-shim.cmd"));

            let output = resolve_command(&resolved).output().unwrap();
            assert!(output.status.success());
            assert_eq!(
                String::from_utf8_lossy(&output.stdout).trim(),
                "resolved mirrord-shim.cmd"
            );
        }

        #[test]
        fn pathext_order_decides_within_a_directory() {
            let dir = dir_with(&["tool.cmd", "tool.exe"]);
            let pathext = env::var("PATHEXT").unwrap().to_ascii_uppercase();
            let expected = if pathext.find(".EXE") < pathext.find(".CMD") {
                "tool.exe"
            } else {
                "tool.cmd"
            };

            let resolved = resolve(OsStr::new("tool"), path_of(&[dir.path()])).unwrap();
            assert_eq!(resolved, dir.path().join(expected));
        }

        #[test]
        fn earlier_path_entry_wins_over_extension() {
            let first = dir_with(&["tool.cmd"]);
            let second = dir_with(&["tool.exe"]);

            let resolved =
                resolve(OsStr::new("tool"), path_of(&[first.path(), second.path()])).unwrap();
            assert_eq!(resolved, first.path().join("tool.cmd"));
        }

        #[test]
        fn explicit_extension_is_found() {
            let dir = dir_with(&["tool.cmd", "tool.exe"]);

            let resolved = resolve(OsStr::new("tool.cmd"), path_of(&[dir.path()])).unwrap();
            assert_eq!(resolved, dir.path().join("tool.cmd"));
        }

        #[test]
        fn skips_scripts_command_cant_spawn() {
            // `.js`, `.vbs`, ... are in the default `PATHEXT` (`.ps1` is when a user adds it), so
            // `which` finds them, but spawning them fails with "not a valid Win32 application".
            let scripts = dir_with(&["tool.js", "tool.vbs", "tool.wsf", "tool.msc", "tool.ps1"]);
            let shim = dir_with(&["tool.cmd"]);

            let resolved =
                resolve(OsStr::new("tool"), path_of(&[scripts.path(), shim.path()])).unwrap();
            assert_eq!(resolved, shim.path().join("tool.cmd"));

            assert_eq!(
                resolve(OsStr::new("tool"), path_of(&[scripts.path()])),
                None
            );
            assert_eq!(
                resolve(OsStr::new("tool.ps1"), path_of(&[scripts.path()])),
                None
            );
        }

        #[test]
        fn skips_extensionless_file() {
            // `which` returns an extensionless file ahead of `tool.cmd` when it is a binary, but
            // std's own `PATH` search never picks extensionless names (it appends `.exe`).
            let dir = dir_with(&["tool.cmd"]);
            fs::copy(env::current_exe().unwrap(), dir.path().join("tool")).unwrap();

            let resolved = resolve(OsStr::new("tool"), path_of(&[dir.path()])).unwrap();
            assert_eq!(resolved, dir.path().join("tool.cmd"));
        }

        #[test]
        fn paths_are_passed_through() {
            let dir = dir_with(&["tool.cmd"]);
            let absolute = dir.path().join("tool");

            for program in [
                absolute.as_os_str(),
                OsStr::new(r".\tool"),
                OsStr::new("bin/tool"),
                OsStr::new("C:tool"),
                OsStr::new(""),
            ] {
                assert_eq!(resolve(program, path_of(&[dir.path()])), None);
                assert_eq!(resolve_command(program).get_program(), program);
            }
        }

        #[test]
        fn missing_program_falls_back_to_name() {
            let dir = dir_with(&[]);
            let program = "mirrord-command-definitely-missing";

            assert_eq!(resolve(OsStr::new(program), path_of(&[dir.path()])), None);
            assert_eq!(resolve_command(program).get_program(), program);
        }
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    #[test]
    fn same_as_command_new() {
        for program in ["sh", "npm", "./bin/tool", "/usr/bin/env"] {
            assert_eq!(resolve_command(program).get_program(), program);
        }
    }
}
