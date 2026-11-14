//! Runs Rust test binaries with cargo-nextest under Bazel, and writes the Cargo metadata that
//! nextest needs to do so. `//bazel:nextest.bzl` explains the design.
//!
//! The program has two modes, which its first argument names:
//!
//! - `metadata <cargo> <workspace manifest> <output>` runs `cargo metadata --no-deps` for the
//!   workspace, as a build action of `cargo_metadata`. It keeps only what nextest reads, and moves
//!   the workspace to [`WORKSPACE_ROOT`], because its real root is a sandbox path that's gone after
//!   the action.
//! - `test` is the executable of a test, whose `args` start with it. It translates Bazel's test
//!   protocol (<https://bazel.build/reference/test-encyclopedia>) into nextest's options, and runs
//!   nextest. Every argument after `test`, from the target's `args` and `--test_arg`, goes to
//!   `cargo nextest run` as it is, so `--test_arg=--help` prints nextest's help. The mode reads
//!   its own settings from the `BAZEL_NEXTEST_*` variables that the rule sets, because an option
//!   of its own would take the name of an option of nextest, like `--config` or `--test-threads`.
//!
//! It's a Rust program rather than a shell script so that it runs on Windows machines, which may
//! not have a shell.
//!
//! It starts programs with `std::process::Command`, which the workspace's `clippy.toml` bans in
//! favor of `mirrord-command`, whose lookup only matters for bare program names. The runner only
//! starts programs by their paths, and depending on `mirrord-command` would run every test again
//! whenever that crate, or the features that the workspace turns on for it, changed.
#![allow(clippy::disallowed_methods)]

use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use serde_json::{Value, json};

/// The workspace's root in the metadata that the `metadata` mode writes. The path is made up, so
/// that the file is the same on every machine, and each test moves the workspace to its runfiles
/// with `--workspace-remap`.
const WORKSPACE_ROOT: &str = "/bazel-nextest-workspace";

/// The prefix of the variables that the rule passes a test's settings in. The files are runfiles
/// paths, like `_main/Cargo.toml`.
///
/// - `NEXTEST`: the cargo-nextest program.
/// - `CONFIG`: `.config/nextest.toml`.
/// - `CARGO_METADATA`: the output of the `metadata` mode.
/// - `WORKSPACE_MANIFEST`: the `Cargo.toml` at the workspace's root in the runfiles, which nextest
///   checks for.
/// - `TRIPLE`: the platform that the tests run on, like `x86_64-unknown-linux-gnu`.
/// - `TEST_THREADS`: how many tests nextest runs at a time, unless the profile sets it.
/// - `BINARY`: the test binary.
/// - `BINARY_ID`: the binary's ID in nextest, like `mirrord-layer-tests::c_stat`.
/// - `BINARY_NAME`: the name of the binary's Cargo target.
/// - `BINARY_KIND`: the kind of the binary's Cargo target, like `test` or `lib`.
/// - `PACKAGE`: the directory of the binary's Cargo package, like `mirrord/layer-tests`.
const SETTINGS_PREFIX: &str = "BAZEL_NEXTEST_";

/// The fields of a package in `cargo metadata`'s output that nextest reads. nextest checks every
/// `package()` and `binary()` filter in `.config/nextest.toml` against the names of the workspace's
/// packages and their targets, so the file must list all of them, but it reads nothing else of the
/// packages that it doesn't run. Dropping the other fields keeps the file the same when they
/// change, so Bazel doesn't run every test again.
const PACKAGE_FIELDS: &[&str] = &[
    "name",
    "version",
    "id",
    "source",
    "manifest_path",
    "edition",
    "targets",
];

fn main() -> anyhow::Result<ExitCode> {
    let arguments: Vec<OsString> = env::args_os().skip(1).collect();

    match arguments.split_first() {
        Some((mode, [cargo, manifest, output])) if mode == "metadata" => {
            write_metadata(Path::new(cargo), Path::new(manifest), Path::new(output))
        }
        Some((mode, nextest_args)) if mode == "test" => run_tests(nextest_args),
        _ => bail!(
            "usage: nextest-runner metadata <cargo> <workspace manifest> <output>\n       \
             nextest-runner test [<argument for `cargo nextest run`>...]"
        ),
    }
}

/// Returns `path` as the path of a `file://` URL, with forward slashes, and on Windows with a
/// leading slash before the drive.
fn url_path(path: &Path) -> String {
    let path = path.display().to_string().replace('\\', "/");

    if path.starts_with('/') {
        path
    } else {
        format!("/{path}")
    }
}

/// Calls `change` with every string in `value`.
fn change_strings(value: &mut Value, change: &mut impl FnMut(&mut String)) {
    match value {
        Value::String(string) => change(string),

        Value::Array(values) => {
            for value in values {
                change_strings(value, change);
            }
        }

        Value::Object(fields) => {
            for value in fields.values_mut() {
                change_strings(value, change);
            }
        }

        _ => {}
    }
}

/// The `metadata` mode: runs the `cargo` program on the workspace's root `Cargo.toml`, `manifest`,
/// and writes `output`.
fn write_metadata(cargo: &Path, manifest: &Path, output: &Path) -> anyhow::Result<ExitCode> {
    let root = env::current_dir().context("can't read the current directory")?;

    // `--no-deps` describes only the workspace's packages, which `Cargo.toml` files and the target
    // sources that Cargo discovers are enough for, so the action needs neither the network nor
    // Cargo's registry. Cargo writes its cache to `CARGO_HOME`, so that goes to a directory of the
    // action's own.
    let cargo_home = root.join("cargo_home");
    let out = Command::new(root.join(cargo))
        .args([
            "metadata",
            "--no-deps",
            "--offline",
            "--format-version",
            "1",
        ])
        .arg("--manifest-path")
        .arg(root.join(manifest))
        .env("CARGO_HOME", &cargo_home)
        .output()
        .with_context(|| format!("can't run {}", cargo.display()))?;

    if !out.status.success() {
        bail!(
            "cargo metadata failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let _ = fs::remove_dir_all(&cargo_home);

    let mut metadata: Value =
        serde_json::from_slice(&out.stdout).context("cargo metadata's output isn't valid JSON")?;

    let packages = metadata
        .get_mut("packages")
        .and_then(Value::as_array_mut)
        .context("cargo metadata's output has no packages")?;

    for package in packages {
        if let Some(fields) = package.as_object_mut() {
            fields.retain(|field, _| PACKAGE_FIELDS.contains(&field.as_str()));

            // nextest's parser requires a package's dependencies and features, but doesn't need
            // their contents, which change most often of all.
            fields.insert("dependencies".to_owned(), json!([]));
            fields.insert("features".to_owned(), json!({}));
        }
    }

    // The workspace's `[workspace.metadata]` holds other tools' settings, like cargo-shear's, which
    // nextest doesn't read.
    if let Some(workspace_metadata) = metadata.get_mut("metadata") {
        *workspace_metadata = Value::Null;
    }

    // Cargo's package IDs hold the root as a `file://` URL, and the other fields as a path. On
    // Windows, the path's separators become `/`, so that the file is the same on every machine.
    let root_url = format!("file://{}", url_path(&root));
    let workspace_root_url = format!("file://{WORKSPACE_ROOT}");
    let root = root.display().to_string();

    change_strings(&mut metadata, &mut |string| {
        if string.contains(&root_url) {
            *string = string.replace(&root_url, &workspace_root_url);
        } else if let Some(path) = string.strip_prefix(&root) {
            *string = format!("{WORKSPACE_ROOT}{}", path.replace('\\', "/"));
        }
    });

    // Cargo also reads the configs in the directories above the root and in the user's Cargo home,
    // which can set a target directory outside the root, so that path would end up in the file.
    // nextest takes the target directory from its own arguments, so a fixed one keeps the file the
    // same on every machine.
    for field in ["target_directory", "build_directory"] {
        if let Some(directory) = metadata.get_mut(field) {
            *directory = json!(format!("{WORKSPACE_ROOT}/target"));
        }
    }

    fs::write(output, metadata.to_string())
        .with_context(|| format!("can't write {}", output.display()))?;

    Ok(ExitCode::SUCCESS)
}

/// Returns the runfiles directory. Bazel names it in the environment. A copy of the test and its
/// runfiles, run without Bazel, finds them next to the program.
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

/// Returns the file at the runfiles path `path`.
///
/// Runfiles paths separate their components with `/`, which Windows doesn't always accept in the
/// middle of a path with `\`, so each component is joined on its own.
fn runfile(runfiles: &Path, path: &str) -> PathBuf {
    path.split('/')
        .fold(runfiles.to_path_buf(), |runfile, component| {
            runfile.join(component)
        })
}

/// Returns a directory of its own for nextest's files. `bazel test` gives the test a temporary
/// directory, and `bazel run`, or a copy of the runfiles on another machine, doesn't.
fn work_directory() -> anyhow::Result<PathBuf> {
    let base = match env::var_os("TEST_TMPDIR") {
        Some(directory) => PathBuf::from(directory),
        None => {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |time| time.as_nanos());
            env::temp_dir().join(format!("nextest-runner-{}-{nanos}", std::process::id()))
        }
    };

    let work = base.join("nextest");
    fs::create_dir_all(&work).with_context(|| format!("can't create {}", work.display()))?;

    Ok(work)
}

/// Returns the path of the first file under `directory` whose extension is `extension`.
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

/// Returns `program` in a form that Windows can start when its path is long.
///
/// `CreateProcessW` only starts a program with a long path when the path is verbatim: absolute,
/// and prefixed with `\\?\`. The standard library makes long paths verbatim when it opens files,
/// but not when it starts a program, and a program in a runfiles directory easily has a long path,
/// because the directory is deep in Bazel's output tree. A short path stays as it is, because a
/// program may not expect a verbatim path as its own.
///
/// This doesn't help with the working directory of the program, which Windows doesn't take as a
/// verbatim path. The tests' working directories, their packages in the runfiles, stay well below
/// the limit under Bazel's default output root.
fn startable_program(program: PathBuf) -> PathBuf {
    /// The length from which Bazel treats a program's path as long: Windows' `MAX_PATH`, 260,
    /// less the 4 characters of the prefix that Windows adds to the path
    /// (<https://github.com/bazelbuild/bazel/issues/12310>).
    const LONG_PATH_LENGTH: usize = 256;

    if !cfg!(windows) {
        return program;
    }

    // Windows reads the `/` and `..` of a verbatim path literally, so the path has to be
    // normalized, which making it absolute also does.
    let Ok(absolute) = std::path::absolute(&program) else {
        return program;
    };

    let path = absolute.as_os_str();

    // A path that starts with `\\` is verbatim already, or names a network share, which takes
    // another prefix.
    if path.len() < LONG_PATH_LENGTH || path.as_encoded_bytes().starts_with(br"\\") {
        return program;
    }

    let mut verbatim = OsString::from(r"\\?\");
    verbatim.push(path);

    PathBuf::from(verbatim)
}

/// Keeps this program running when it gets a signal that asks it to stop, so that it can still
/// copy the JUnit report after nextest exits.
///
/// When a test times out, Bazel sends SIGTERM to this program and to nextest, which are in one
/// process group, and kills what's left of the group after a grace period
/// (`--local_termination_grace_seconds`, 15 seconds by default). `bazel run` does the same with
/// SIGINT on Ctrl-C. nextest reacts by stopping the tests that are running, and it then writes the
/// JUnit report with the tests that finished. The default reaction of this program would be to exit
/// before nextest has written the report, and Bazel would then write a `test.xml` with a single
/// test case for the whole target.
///
/// The handler does nothing, rather than the signal being ignored, because a process that this one
/// starts inherits the signals that it ignores, but not its handlers. nextest and the tests must
/// still get the signals. Windows has no such signals: Bazel ends a test there without one.
#[cfg(unix)]
fn outlive_termination_signals() {
    extern "C" fn do_nothing(_signal: libc::c_int) {}

    let handler = do_nothing as extern "C" fn(libc::c_int);

    for signal in [libc::SIGHUP, libc::SIGINT, libc::SIGTERM] {
        // SAFETY: `do_nothing` has the signature of a signal handler, and does nothing, which is
        // safe at any point of the program.
        unsafe { libc::signal(signal, handler as libc::sighandler_t) };
    }
}

/// Returns how many seconds nextest may run the tests for, so that it ends the run before Bazel's
/// timeout for the target does, or `None` outside of `bazel test`.
///
/// When the time is up, nextest stops the tests that are still running, waits for them for the
/// profile's `slow-timeout.grace-period` (10 seconds by default), and writes the JUnit report,
/// which names the tests that it stopped. Bazel would stop nextest with a signal and keep its
/// report, but only on Unix: on Windows, Bazel kills the test without a signal, and the report is
/// lost. The margin also leaves room for the grace period, unless the profile sets a longer one.
fn global_timeout() -> Option<u64> {
    /// The most time that nextest gets to stop the tests and write the report.
    const MARGIN: u64 = 15;

    let timeout: u64 = env::var("TEST_TIMEOUT").ok()?.parse().ok()?;

    Some(timeout - MARGIN.min(timeout / 4))
}

/// Returns the setting `name` of a test, from the variable that the rule passes it in (see
/// [`SETTINGS_PREFIX`]).
fn setting(name: &str) -> anyhow::Result<String> {
    let variable = format!("{SETTINGS_PREFIX}{name}");

    env::var(&variable).with_context(|| {
        format!("{variable} isn't set, which the rule of a test sets for the `test` mode")
    })
}

/// The test mode.
fn run_tests(nextest_args: &[OsString]) -> anyhow::Result<ExitCode> {
    let runfiles = runfiles_directory()?;
    let nextest = runfile(&runfiles, &setting("NEXTEST")?);
    let config = runfile(&runfiles, &setting("CONFIG")?);
    let cargo_metadata = runfile(&runfiles, &setting("CARGO_METADATA")?);
    let workspace_manifest = runfile(&runfiles, &setting("WORKSPACE_MANIFEST")?);
    let work = work_directory()?;

    // nextest runs the tests in the directory of their package, which `--workspace-remap` places
    // in the runfiles.
    let workspace_root = workspace_manifest
        .parent()
        .context("the workspace's manifest has no directory")?
        .to_path_buf();

    let metadata = fs::read(&cargo_metadata)
        .with_context(|| format!("can't read {}", cargo_metadata.display()))?;

    let metadata: Value = serde_json::from_slice(&metadata)
        .with_context(|| format!("{} isn't valid JSON", cargo_metadata.display()))?;

    // Cargo's ID of the package that the binary belongs to, which starts with the package's
    // directory.
    let package = setting("PACKAGE")?;
    let package_id_prefix = format!("path+file://{WORKSPACE_ROOT}/{package}#");
    let package_id = metadata
        .get("packages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|package| package.get("id").and_then(Value::as_str))
        .find(|id| id.starts_with(&package_id_prefix))
        .with_context(|| format!("{package} isn't a package of the Cargo workspace"))?;

    // The list of test binaries that `cargo nextest list --list-type binaries-only` would write
    // after a build.
    let binary_id = setting("BINARY_ID")?;
    let test_binaries = json!({
        binary_id.as_str(): {
            "binary-id": binary_id,
            "binary-name": setting("BINARY_NAME")?,
            "package-id": package_id,
            "kind": setting("BINARY_KIND")?,
            "binary-path": runfile(&runfiles, &setting("BINARY")?),
            "build-platform": "target",
        },
    });

    // nextest puts the toolchain's library directory on the tests' library path, for a standard
    // library that's linked dynamically. Bazel links it statically, so the directory is an empty
    // one. nextest would warn about a missing one.
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

    // Settings that take precedence over nextest's defaults, but not over `.config/nextest.toml`.
    // nextest writes its store, including the JUnit report, under the workspace's root by
    // default, which is in the runfiles, so it writes into the work directory instead. Every
    // profile writes a JUnit report, unless `.config/nextest.toml` says otherwise, because the
    // report becomes the target's `test.xml`. TOML's literal strings take Windows paths as they
    // are.
    let store = work.join("store");
    let tool_config = work.join("bazel.toml");
    let mut profile = format!("test-threads = {}\n", setting("TEST_THREADS")?);

    if let Some(seconds) = global_timeout() {
        profile.push_str(&format!("global-timeout = \"{seconds}s\"\n"));
    }

    fs::write(
        &tool_config,
        format!(
            "[store]\ndir = '{}'\n\n[profile.default]\n{profile}\n[profile.default.junit]\npath = \"junit.xml\"\n",
            store.display()
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

    // Bazel's test setup puts the working directory, `.`, first in `PATH`, and nextest runs each
    // test in its package's directory, which holds the package's test binaries. A program that the
    // test runs by name would then find a test binary of the same name, like the layer tests'
    // `bash`, so the tests get the `PATH` without it, as they do under Cargo.
    if let Some(path) = env::var_os("PATH") {
        let path = env::split_paths(&path).filter(|directory| directory != Path::new("."));
        command.env("PATH", env::join_paths(path)?);
    }

    // Bazel's test setup exports a bash function, `rlocation`, whose body contains the text "ERROR:
    // rlocation is no longer implicitly provided". Every process of the tests inherits it as the
    // variable `BASH_FUNC_rlocation%%`. The CLI logs its environment at debug level, so the
    // end-to-end tests that fail on `ERROR` in the CLI's output would fail.
    command.env_remove("BASH_FUNC_rlocation%%");

    // With `shard_count`, Bazel runs the target once for each shard. nextest sorts the tests and
    // splits them between the shards in turn (`slice:`), so the shards hold the same number of
    // tests, give or take one. `hash:` would keep a test in its shard when other tests are added,
    // but that doesn't help here: every shard takes the same binary, so Bazel reruns all of the
    // shards when a test is added, and hashing leaves the shards of a small binary uneven. The
    // runner touches the status file because Bazel fails a sharded test that doesn't, on the
    // assumption that such a test runs every test in each shard.
    if let (Ok(total), Ok(index)) = (env::var("TEST_TOTAL_SHARDS"), env::var("TEST_SHARD_INDEX")) {
        if let Some(status_file) = env::var_os("TEST_SHARD_STATUS_FILE") {
            fs::write(&status_file, "")
                .with_context(|| format!("can't write {}", Path::new(&status_file).display()))?;
        }

        let index: u32 = index.parse().context("TEST_SHARD_INDEX isn't a number")?;

        command
            .arg("--partition")
            .arg(format!("slice:{}/{total}", index + 1));
    }

    // `--test_runner_fail_fast` stops at the first test that fails, like nextest's `--fail-fast`.
    // Without it, nextest's own default applies, as under `cargo nextest run`. That default also
    // stops at the first failure, unlike Bazel's other test runners.
    if env::var("TESTBRIDGE_TEST_RUNNER_FAIL_FAST").as_deref() == Ok("1") {
        command.arg("--fail-fast");
    }

    // `--test_filter` runs only the tests whose names contain it, as libtest's filter would. It
    // comes before the arguments from `--test_arg`, which may end with `--` and arguments for the
    // test binary.
    if let Ok(filter) = env::var("TESTBRIDGE_TEST_ONLY") {
        command.arg(filter);
    }

    command.args(nextest_args);

    // A target without tests passes, as a libtest binary without tests does. That includes a
    // shard without tests, and a target that `--test_filter` leaves no tests in.
    if env::var_os("NEXTEST_NO_TESTS").is_none() {
        command.env("NEXTEST_NO_TESTS", "pass");
    }

    // The tests find their data in the runfiles directory (see `env_path` in `mirrord-test-utils`),
    // which they can't find on their own, because nextest runs each test in its package's directory
    // rather than in the runfiles' root. The settings are this program's own, so the tests don't
    // inherit them.
    command.env("RUNFILES_DIR", &runfiles);

    for (name, _) in env::vars_os() {
        if name.to_string_lossy().starts_with(SETTINGS_PREFIX) {
            command.env_remove(name);
        }
    }

    #[cfg(unix)]
    outlive_termination_signals();

    // nextest runs from the work directory, so that it doesn't read the workspace's
    // `.cargo/config.toml`, which configures Cargo's builds.
    let status = command
        .current_dir(&work)
        .status()
        .with_context(|| format!("can't run {}", nextest.display()))?;

    // Bazel keeps the JUnit report as the target's `test.xml`. Without it, Bazel would write a
    // report with a single test case for the whole target. The store holds no other XML file, so
    // the report is found by its extension, whatever `.config/nextest.toml` names it.
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
