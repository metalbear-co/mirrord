//! Finds the files in a Bazel program's runfiles, the directory where Bazel lays out the files of
//! the program's `data` by their runfiles paths (`$(rlocationpath)`).
//!
//! The CLI reads the files that it bundles, like the layer, from its runfiles, unless it's built
//! with `standalone`, and the tests find the CLI, their apps and their data files in theirs.
//!
//! [`rlocation!`] takes any file's runfiles path. The functions take a file's path in mirrord's
//! `mirrord` directory, for mirrord's own crates. A file that differs between platforms gets such a
//! path from a `runfiles_link`.

use std::{path::PathBuf, sync::LazyLock};

use runfiles::Runfiles;

/// This program's runfiles directory, and the runfiles library that looks files up in it.
///
/// The process keeps both, so that it reads the runfiles' `_repo_mapping` file only once.
///
/// The runfiles library would by default return a file's real path from the runfiles manifest,
/// which is outside the runfiles directory. But Node resolves a script's imports from the directory
/// of the script's path, and only the runfiles directory has `//:node_modules` (see the layer
/// tests' `--preserve-symlinks-main`). So the library gets the directory, and never reads the manifest.
static RUNFILES: LazyLock<(PathBuf, Runfiles)> = LazyLock::new(|| {
    // Bazel sets `RUNFILES_DIR` when it runs a test, but unsets it for `bazel run`. A program that
    // `bazel run` starts, or that a dev runs from `bazel-bin`, has its runfiles in the
    // `<program>.runfiles` directory next to it.
    let directory = std::env::var_os("RUNFILES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let program = std::env::current_exe().expect("the program knows its own path");
            let mut runfiles = program.into_os_string();
            runfiles.push(".runfiles");
            PathBuf::from(runfiles)
        });

    let runfiles = Runfiles::builder()
        .directory(&directory)
        .build()
        .expect("the runfiles' `_repo_mapping` is valid");

    (directory, runfiles)
});

/// Returns the absolute path of the file at `runfiles_path` in this program's runfiles.
///
/// The runfiles path starts with the name of a repository as the crate that calls the macro sees
/// it, which for a module is the module's name: `mirrord/mirrord/cli/mirrord` is the CLI in
/// mirrord's module. The rest is the file's path in that repository.
///
/// The macro reads `REPOSITORY_NAME`, the canonical name of the repository that Bazel builds the
/// calling crate in, which rules_rust passes to every crate that it compiles. The name is empty in
/// the main repository, and under Cargo, which doesn't set the variable. The runfiles library maps
/// the path's first part to a directory of the runfiles as that repository sees it. `mirrord` is
/// `_main` when mirrord is the main repository, and `mirrord+` when another module depends on it.
///
/// So the calling crate's repository must see the repository that the path names. When another
/// module's crate hub builds the crate, from a Cargo git dependency, the crate is in a repository
/// of rules_rs's `crate` extension, which only sees the extension's repositories and the ones that
/// the root module injects. That module must then have `inject_repo(crate, "<name>")` in its
/// `MODULE.bazel`.
///
/// # Panics
///
/// When the file isn't in the runfiles, because the program's `data` must list it.
#[macro_export]
macro_rules! rlocation {
    ($runfiles_path:expr) => {
        $crate::rlocation_from(
            $runfiles_path,
            match ::core::option_env!("REPOSITORY_NAME") {
                ::core::option::Option::Some(repository) => repository,
                ::core::option::Option::None => "",
            },
        )
    };
}

/// Returns the absolute path of the file at `runfiles_path` in this program's runfiles, where the
/// repository with the canonical name `repository` sees the path's first part as the name of a
/// repository.
///
/// Call [`rlocation!`] instead, which passes the repository of the crate that calls it.
///
/// # Panics
///
/// When the file isn't in the runfiles, because the program's `data` must list it.
pub fn rlocation_from(runfiles_path: &str, repository: &str) -> PathBuf {
    let (directory, runfiles) = &*RUNFILES;

    let path = runfiles
        .rlocation_from(runfiles_path, repository)
        .filter(|path| path.exists());
    let Some(path) = path else {
        panic!(
            "{runfiles_path} isn't in the program's runfiles at {}",
            directory.display()
        );
    };

    // `RUNFILES_DIR` can be relative, and the program may start other programs in another working
    // directory.
    std::path::absolute(path).expect("the working directory exists")
}

/// Returns the absolute path of the file at `path` in the repository's `mirrord` directory, like
/// `layer-tests/configs/fs.json`, in this program's runfiles.
///
/// # Panics
///
/// When the file isn't in the runfiles, because the program's `data` must list it.
pub fn runfile(path: &str) -> PathBuf {
    // The path's first part is the name of mirrord's module.
    rlocation!(&format!("mirrord/mirrord/{path}"))
}

/// Returns the absolute path of the program at `path` in the repository's `mirrord` directory, like
/// `cli/mirrord` for the CLI that `//mirrord/cli` builds, in this program's runfiles.
///
/// The path has no `.exe`, which Bazel adds to every program's name on Windows.
///
/// # Panics
///
/// When the program isn't in the runfiles, because the program's `data` must list it.
pub fn runfile_program(path: &str) -> PathBuf {
    runfile(&format!("{path}{}", std::env::consts::EXE_SUFFIX))
}
