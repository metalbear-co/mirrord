//! Finds the files that the CLI bundles to load the layer in Bazel's runfiles.
//!
//! The CLI embeds the files that it bundles, like the layer and the UI, at compile time by default,
//! which is how Cargo builds it, and how Bazel builds the binary that mirrord distributes,
//! `//mirrord/cli:cli_standalone`. Bazel's `//mirrord/cli`, which turns on the `bazel-runfiles`
//! feature, reads the layer's files from its runfiles at run time instead, so that changing the
//! layer doesn't compile the CLI again, and has no UI.
//!
//! Each file comes from an environment variable that the build passes to rustc: the file's path
//! when the CLI embeds it, from `.cargo/config.toml` or Bazel's `$(execpath)`, and its runfiles
//! path, from Bazel's `$(rlocationpath)`, when the CLI reads it at run time.

use std::path::PathBuf;

/// Returns the absolute path of the file at `runfiles_path` in the CLI's runfiles.
///
/// The runfiles are in `RUNFILES_DIR` when Bazel runs the CLI, like in a test or with `bazel run`,
/// and in a directory next to the CLI, named after it with `.runfiles`, when someone runs the CLI
/// from `bazel-bin` directly.
pub(crate) fn runfile(runfiles_path: &str) -> PathBuf {
    let runfiles = std::env::var_os("RUNFILES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let program = std::env::current_exe().expect("the program knows its own path");
            let mut runfiles = program.into_os_string();
            runfiles.push(".runfiles");
            PathBuf::from(runfiles)
        });

    let path = runfiles.join(runfiles_path);
    assert!(
        path.exists(),
        "{runfiles_path} isn't in the program's runfiles at {}",
        runfiles.display()
    );

    // `RUNFILES_DIR` can be relative to the working directory, which the program may change.
    std::path::absolute(path).expect("the working directory exists")
}
