//! The program that runs the Rust test binaries that Bazel built under cargo-nextest. It's the
//! executable of every `rust_nextest_test`, which runs one binary, and of every
//! `rust_nextest_suite`, which runs the binaries of several tests in one run.
//!
//! Our tests need nextest rather than libtest, which rules_rust's `rust_test` runs directly:
//!
//! - libtest runs all of a crate's tests on threads of one process, and some of our tests change
//!   the process's environment or working directory, which would race with the other tests.
//!   nextest runs each test in a process of its own.
//! - Our tests rely on the retries and timeouts in `.config/nextest.toml`.
//! - nextest's JUnit report gives Bazel a result for each test, rather than a single one for the
//!   whole target.
//!
//! nextest only supports Cargo builds, and its maintainer declined to add Bazel support
//! (<https://github.com/nextest-rs/nextest/issues/842>). They said that an integrated solution
//! would be a lot of work, and pointed to the `--cargo-metadata` and `--binaries-metadata` options
//! of `cargo nextest run` instead (<https://github.com/nextest-rs/nextest/discussions/2421>).
//!
//! The two options take the output of `cargo metadata` and the list of test binaries that a Cargo
//! build would write (<https://nexte.st/docs/ci-features/archiving/>). `@cargo_packages` writes
//! the first file for the whole workspace, and the runner writes the second for the target's test
//! binaries. It then maps Bazel's test protocol (<https://bazel.build/reference/test-encyclopedia>)
//! to nextest's options and runs `cargo nextest run`. The runner's arguments go to nextest
//! unchanged, so `--test_arg=--help` prints nextest's help.
//!
//! The runner is a Rust program rather than a shell script because Windows machines don't always
//! have a shell.

// The workspace's `clippy.toml` bans `std::process::Command` in favor of `mirrord-command`, which
// finds the `.cmd` and `.bat` shims of bare program names on Windows. The runner starts programs
// by their paths, so it doesn't need that.
//
// Every test depends on the runner, so depending on `mirrord-command` would also rerun every test
// whenever that crate changed.
#![allow(clippy::disallowed_methods)]

use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use anyhow::Context;
use serde::Deserialize;
use serde_json::json;
use tempfile::TempDir;

/// The prefix of the environment variables that `rust_nextest_test` and `rust_nextest_suite` pass
/// the runner's settings in, like `BAZEL_NEXTEST_BINARIES`. The settings aren't command-line
/// options, because every argument goes to nextest, and an option of the runner could have the same
/// name as one of nextest's, like `--config`.
const SETTINGS_PREFIX: &str = "BAZEL_NEXTEST_";

/// Bazel names the runfiles directory in the environment, and a test that runs outside Bazel finds
/// its runfiles next to the program, in `<program>.runfiles`.
fn runfiles_directory() -> anyhow::Result<PathBuf> {
    env::var_os("RUNFILES_DIR")
        .or_else(|| env::var_os("TEST_SRCDIR"))
        .map(PathBuf::from)
        .or_else(|| {
            env::args_os()
                .next()
                .map(|program| PathBuf::from(format!("{}.runfiles", program.to_string_lossy())))
        })
        .context("the runfiles aren't in the environment or next to the program")
}

/// Returns the file at the runfiles path `path`, like `_main/Cargo.toml`.
///
/// Windows doesn't always accept `/` in a path that also has `\`, so each component of `path` is
/// joined on its own.
fn runfile(runfiles: &Path, path: &str) -> PathBuf {
    path.split('/')
        .fold(runfiles.to_path_buf(), |runfile, component| {
            runfile.join(component)
        })
}

/// Creates a directory for nextest's files, in the test's temporary directory (`TEST_TMPDIR`), or
/// in the system's one when the test runs outside `bazel test`. The directory is deleted when the
/// returned value is dropped.
fn work_directory() -> anyhow::Result<TempDir> {
    let base = env::var_os("TEST_TMPDIR").map_or_else(env::temp_dir, PathBuf::from);
    tempfile::Builder::new()
        .prefix("nextest-")
        .tempdir_in(&base)
        .with_context(|| format!("can't create a directory in {}", base.display()))
}

fn find_file(directory: &Path, extension: &str) -> Option<PathBuf> {
    for entry in fs::read_dir(directory).ok()?.flatten() {
        let path = entry.path();

        if path.is_dir() {
            if let Some(found) = find_file(&path, extension) {
                return Some(found);
            }
        } else if path.extension().is_some_and(|found| found == extension) {
            return Some(path);
        }
    }

    None
}

/// Returns `program` in a form that Windows can start even when its path is long, as the paths in
/// the runfiles often are. Windows only starts a program with a long path when the path is
/// verbatim (`\\?\`), and the standard library adds that prefix when it opens a file but not when
/// it starts a program.
///
/// A short path stays unchanged, because a program may not expect a verbatim path as its own.
fn startable_program(program: PathBuf) -> PathBuf {
    /// The length from which a program's path counts as long: `MAX_PATH`, 260, less the 4
    /// characters of the verbatim prefix, as Bazel counts it
    /// (<https://github.com/bazelbuild/bazel/issues/12310>).
    const LONG_PATH_LENGTH: usize = 256;

    if !cfg!(windows) {
        return program;
    }

    // Windows reads `/` and `..` literally in a verbatim path, so `absolute` normalizes it first.
    let Ok(absolute) = std::path::absolute(&program) else {
        return program;
    };

    let path = absolute.as_os_str();

    // A path that starts with `\\` is either verbatim already or a network share, which takes the
    // `\\?\UNC\` prefix instead.
    if path.len() < LONG_PATH_LENGTH || path.as_encoded_bytes().starts_with(br"\\") {
        return program;
    }

    let mut verbatim = OsString::from(r"\\?\");
    verbatim.push(path);

    PathBuf::from(verbatim)
}

/// Keeps this program running when Bazel stops the test, so that it can still copy nextest's JUnit
/// report. When a test times out, Bazel sends SIGTERM to the test's process group, and nextest then
/// stops the tests and writes its report.
#[cfg(unix)]
fn outlive_termination_signals() {
    // `execve` keeps ignored signals ignored but resets handlers to the default, so with a handler
    // rather than `SIG_IGN`, nextest and the tests still react to these signals.
    extern "C" fn do_nothing(_signal: libc::c_int) {}

    let handler = do_nothing as extern "C" fn(libc::c_int);

    for signal in [libc::SIGHUP, libc::SIGINT, libc::SIGTERM] {
        // SAFETY: `do_nothing` is a valid signal handler, and doing nothing is async-signal-safe.
        unsafe { libc::signal(signal, handler as libc::sighandler_t) };
    }
}

/// Returns nextest's `global-timeout` in seconds, which ends the run shortly before Bazel's timeout
/// for the target, or `None` outside `bazel test`.
///
/// When the global timeout runs out, nextest stops the tests that are still running and writes a
/// JUnit report that names them. Bazel's timeout alone would lose that report on Windows, because
/// Bazel kills a test there without a signal that nextest could react to.
fn global_timeout() -> Option<u64> {
    /// The most seconds that nextest gets to stop the tests and write the report. Stopping a test
    /// waits for the profile's `slow-timeout.grace-period`, 10 seconds by default.
    const MARGIN: u64 = 15;

    let timeout: u64 = env::var("TEST_TIMEOUT").ok()?.parse().ok()?;

    Some(timeout - MARGIN.min(timeout / 4))
}

/// A test binary of the run, as `NextestInfo` in `nextest.bzl` describes it.
#[derive(Deserialize)]
struct TestBinary {
    /// The binary's ID in nextest, like `mirrord-layer-tests::c_stat`.
    id: String,
    /// The kind of the binary's Cargo target, like `test` or `lib`.
    kind: String,
    /// The name of the binary's Cargo target.
    name: String,
    /// The `package_id` of the binary's Cargo package in the workspace's metadata.
    package_id: String,
    /// The binary's path in the runfiles.
    path: String,
}

fn setting(name: &str) -> anyhow::Result<String> {
    let variable = format!("{SETTINGS_PREFIX}{name}");

    env::var(&variable).with_context(|| {
        format!("{variable} isn't set, which the rule of a test sets for the `test` mode")
    })
}

fn main() -> anyhow::Result<ExitCode> {
    let runfiles = runfiles_directory()?;
    let nextest = runfile(&runfiles, &setting("NEXTEST")?);
    let config = runfile(&runfiles, &setting("CONFIG")?);
    let cargo_metadata = runfile(&runfiles, &setting("CARGO_METADATA")?);
    let workspace_manifest = runfile(&runfiles, &setting("WORKSPACE_MANIFEST")?);
    // Kept until the end of `main`, which copies nextest's report out of it.
    let work_directory = work_directory()?;
    let work = work_directory.path();

    let workspace_root = workspace_manifest
        .parent()
        .context("the workspace's manifest has no directory")?
        .to_path_buf();

    let binaries: Vec<TestBinary> = serde_json::from_str(&setting("BINARIES")?)
        .with_context(|| format!("{SETTINGS_PREFIX}BINARIES isn't a list of test binaries"))?;

    // What `cargo nextest list --list-type binaries-only` would write after a build.
    let test_binaries: serde_json::Map<String, serde_json::Value> = binaries
        .into_iter()
        .map(|binary| {
            let entry = json!({
                "binary-id": binary.id,
                "binary-name": binary.name,
                "package-id": binary.package_id,
                "kind": binary.kind,
                "binary-path": runfile(&runfiles, &binary.path),
                "build-platform": "target",
            });
            (binary.id, entry)
        })
        .collect();

    // nextest adds the toolchain's library directory to the tests' library path, and warns when
    // it's missing. Bazel links the standard library statically, so an empty directory is enough.
    let libdir = work.join("libdir");
    fs::create_dir_all(&libdir).with_context(|| format!("can't create {}", libdir.display()))?;

    let platform = json!({"triple": setting("TRIPLE")?, "target-features": "unknown"});

    let binaries_metadata = json!({
        "rust-build-meta": {
            "target-directory": work.join("target"),
            "base-output-directories": [],
            "non-test-binaries": {},
            "build-script-out-dirs": {},
            "linked-paths": [],
            "platforms": {
                "host": {
                    "platform": platform,
                    "libdir": {"status": "available", "path": libdir},
                },
                "targets": [],
            },
            "target-platforms": [platform],
        },
        "rust-binaries": test_binaries,
    });

    let binaries_metadata_path = work.join("binaries-metadata.json");
    fs::write(&binaries_metadata_path, binaries_metadata.to_string())
        .with_context(|| format!("can't write {}", binaries_metadata_path.display()))?;

    // nextest's store, which holds the JUnit report, defaults to a directory under the workspace's
    // root, in the runfiles. A test shouldn't write to its runfiles
    // (<https://bazel.build/reference/test-encyclopedia>), so the store goes to the work directory.
    let store = work.join("store");

    // Defaults that `.config/nextest.toml` can override.
    let tool_config = work.join("bazel.toml");
    let mut profile = format!("test-threads = {}\n", setting("TEST_THREADS")?);

    if let Some(seconds) = global_timeout() {
        profile.push_str(&format!("global-timeout = \"{seconds}s\"\n"));
    }

    // The store's path is a TOML string that `toml` escapes, since a path can hold any character,
    // like the `\` of a Windows path or the `'` of a user's name.
    fs::write(
        &tool_config,
        format!(
            "[store]\ndir = {}\n\n[profile.default]\n{profile}\n[profile.default.junit]\npath = \"junit.xml\"\n",
            toml::Value::String(store.display().to_string()),
        ),
    )
    .with_context(|| format!("can't write {}", tool_config.display()))?;

    let mut command = Command::new(startable_program(nextest.clone()));

    command
        .args(["nextest", "run"])
        .arg("--cargo-metadata")
        .arg(&cargo_metadata)
        .arg("--binaries-metadata")
        .arg(&binaries_metadata_path)
        .arg("--workspace-remap")
        .arg(&workspace_root)
        .arg("--config-file")
        .arg(config)
        .arg("--tool-config-file")
        .arg(format!("bazel:{}", tool_config.display()));

    // Bazel's test setup puts `.` first in `PATH`, and nextest runs each test in its package's
    // directory, where a test binary like the layer tests' `bash` would shadow the real program.
    if let Some(path) = env::var_os("PATH") {
        let path = env::split_paths(&path).filter(|directory| directory != Path::new("."));
        command.env("PATH", env::join_paths(path)?);
    }

    if let (Ok(total), Ok(index)) = (env::var("TEST_TOTAL_SHARDS"), env::var("TEST_SHARD_INDEX")) {
        // Touching the status file tells Bazel that the test supports sharding, and Bazel fails a
        // shard that doesn't touch it.
        if let Some(status_file) = env::var_os("TEST_SHARD_STATUS_FILE") {
            fs::write(&status_file, "")
                .with_context(|| format!("can't write {}", Path::new(&status_file).display()))?;
        }

        let index: u32 = index.parse().context("TEST_SHARD_INDEX isn't a number")?;

        // `slice:` splits the tests evenly between the shards, and `hash:` would leave the shards
        // of a binary with few tests uneven.
        command
            .arg("--partition")
            .arg(format!("slice:{}/{total}", index + 1));
    }

    // Without Bazel's `--test_runner_fail_fast`, nextest's default applies, which also fails fast.
    if env::var("TESTBRIDGE_TEST_RUNNER_FAIL_FAST").as_deref() == Ok("1") {
        command.arg("--fail-fast");
    }

    // Bazel's `--test_filter` becomes nextest's filter, which must come before the arguments from
    // `--test_arg`, because they may end with `--` and arguments for the test binary.
    if let Ok(filter) = env::var("TESTBRIDGE_TEST_ONLY") {
        command.arg(filter);
    }

    command.args(env::args_os().skip(1));

    // nextest fails a run without tests, but a target, shard or `--test_filter` without tests
    // should pass, as a libtest binary without tests does.
    if env::var_os("NEXTEST_NO_TESTS").is_none() {
        command.env("NEXTEST_NO_TESTS", "pass");
    }

    // The tests find their data in the runfiles (`mirrord-runfiles`). nextest runs
    // each test in its package's directory rather than in the runfiles' root, so the tests can't
    // find the runfiles on their own.
    command.env("RUNFILES_DIR", &runfiles);

    for (name, _) in env::vars_os() {
        if name.to_string_lossy().starts_with(SETTINGS_PREFIX) {
            command.env_remove(name);
        }
    }

    #[cfg(unix)]
    outlive_termination_signals();

    // nextest runs from the work directory, so that it doesn't read the workspace's
    // `.cargo/config.toml`, which is for Cargo builds.
    let status = command
        .current_dir(work)
        .status()
        .with_context(|| format!("can't run {}", nextest.display()))?;

    // Bazel keeps the file at `XML_OUTPUT_FILE` as the target's `test.xml`, and otherwise writes
    // one with a single test case for the whole target.
    //
    // The store holds no other XML file, so this finds nextest's report whatever
    // `.config/nextest.toml` names it.
    if let (Some(xml_output), Some(report)) =
        (env::var_os("XML_OUTPUT_FILE"), find_file(&store, "xml"))
    {
        fs::copy(&report, &xml_output).with_context(|| {
            format!(
                "can't copy {} to {}",
                report.display(),
                Path::new(&xml_output).display()
            )
        })?;
    }

    Ok(match status.code() {
        Some(0) => ExitCode::SUCCESS,
        Some(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        None => ExitCode::FAILURE,
    })
}
