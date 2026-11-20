#![allow(clippy::unused_io_amount)]
#![allow(clippy::indexing_slicing)]

use core::ops::Not;
#[cfg(not(target_os = "windows"))]
use std::os::unix::process::ExitStatusExt;
use std::{
    collections::HashMap,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use chrono::{Timelike, Utc};
use fancy_regex::Regex;
use mirrord_command::{resolve_command, resolve_tokio_command};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
    process::Child,
    sync::RwLock,
    task::JoinHandle,
};

// The tests find the programs and files in their `data` with these.
pub use mirrord_runfiles::{runfile, runfile_program};

pub mod run_command;
#[cfg(all(test, unix))]
mod wait_for_line_tests;

/// Directory this harness keeps a copy of every process's output in, one file per process
/// named `<label>-<pid>.log`, when set. With it, what an app run under mirrord writes can be read
/// while the test runs, next to the logs of the pods in the cluster, instead of only in nextest's
/// report of a failure. Nothing is written when it is unset.
pub const LOG_DIR_ENV: &str = "MIRRORD_TESTS_LOG_DIR";

/// Returns the Node.js that runs the Node apps, so that the tests don't depend on the machine's
/// `node`.
///
/// The test must have `//mirrord/test-utils:node` in its runfiles, which the package of each Node
/// app gives it.
pub fn node() -> String {
    mirrord_runfiles::runfile_program("test-utils/node")
        .to_string_lossy()
        .into_owned()
}

/// The Python interpreter that Bazel provides for the Python apps, and the path of their packages.
struct BazelPython {
    /// The path of the interpreter.
    executable: String,

    /// The `PYTHONPATH` that holds the apps' pip packages.
    python_path: String,
}

/// The Python interpreter that Bazel provides, once a test asks for it.
static BAZEL_PYTHON: OnceLock<BazelPython> = OnceLock::new();

/// Returns the Python interpreter that Bazel provides.
///
/// The test must have `//mirrord/test-utils:python_env` in its runfiles, which the package of each
/// Python app gives it. It's a program that prints the interpreter and the path of the apps' pip
/// packages. The tests can't run the apps with that
/// program, because Bazel starts a Python program with a shell script, and the layer would load
/// into the shell and make calls that the tests don't expect. So the tests run the interpreter
/// directly, with that path in `PYTHONPATH`.
fn bazel_python() -> &'static BazelPython {
    BAZEL_PYTHON.get_or_init(|| {
        let python_env = mirrord_runfiles::runfile_program("test-utils/python_env");
        // On Windows, rules_python deletes the program's virtual environment, which holds the
        // interpreter that the program prints, when the program exits, unless
        // `RULES_PYTHON_EXTRACT_ROOT` is set.
        //
        // Each test process needs its own directory, because the tests run in parallel, and
        // the program rewrites the virtual environment's configuration each time it runs.
        let venv_root = std::env::var_os("TEST_TMPDIR")
            .map_or_else(std::env::temp_dir, PathBuf::from)
            .join(format!("python-env-{}", std::process::id()));
        let output = resolve_command(python_env)
            .env("RULES_PYTHON_EXTRACT_ROOT", venv_root)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        let mut lines = stdout.lines().map(str::to_owned);
        BazelPython {
            executable: lines.next().unwrap(),
            python_path: lines.next().unwrap(),
        }
    })
}

/// Returns the Python interpreter that Bazel provides for the Python apps, so that the tests don't
/// depend on the machine's `python3` and its packages.
///
/// The interpreter only finds the apps' pip packages with [`python_path`] in `PYTHONPATH`, which
/// [`run_command::run_exec`] sets.
pub fn python() -> String {
    bazel_python().executable.clone()
}

/// Returns the `PYTHONPATH` that the interpreter of [`python`] needs, or `None` when the test
/// hasn't called [`python`].
///
/// A test builds an app's command, and so calls [`python`], before it runs the app. A test that
/// hasn't called it runs no Python app, and shouldn't wait for `:python_env` to start.
pub fn python_path() -> Option<&'static str> {
    BAZEL_PYTHON.get().map(|python| python.python_path.as_str())
}

/// The file a process's output is copied to under [`LOG_DIR_ENV`], or `None` when the
/// directory is unset or cannot be written; a copy that fails must not fail the test.
fn output_copy(label: &str, pid: u32) -> Option<Arc<Mutex<File>>> {
    let dir = PathBuf::from(std::env::var_os(LOG_DIR_ENV)?);
    std::fs::create_dir_all(&dir).ok()?;
    let file = File::create(dir.join(format!("{label}-{pid}.log"))).ok()?;
    Some(Arc::new(Mutex::new(file)))
}

/// Appends one chunk of a process's output to its copy, prefixed like the terminal line.
fn copy_output(copy: &Option<Arc<Mutex<File>>>, stream: &str, chunk: &str) {
    if let Some(copy) = copy
        && let Ok(mut file) = copy.lock()
    {
        let _ = write!(file, "{stream} {} {chunk}", format_time());
    }
}

/// The name a process's output copy is filed under: what runs, so a `mirrord exec ... --
/// /path/to/app` is filed as `app`, not `mirrord`. The label is the stem of the program's file
/// name, which is the app's name for every test app, including a Go app's build for each Go
/// version, named `<app>.<minor>` like `go-e2e-dir.27`, and the C app that the layer tests build
/// against another glibc, named `<app>.out`.
pub fn output_label<S: AsRef<str>>(program: &str, args: &[S]) -> String {
    let executable = args
        .iter()
        .position(|arg| arg.as_ref() == "--")
        .and_then(|separator| args.get(separator + 1))
        .map(AsRef::as_ref)
        .unwrap_or(program);
    Path::new(executable)
        .file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "process".to_owned())
}

#[cfg(test)]
mod output_label_tests {
    use super::*;

    #[test]
    fn output_is_filed_under_the_app_not_the_launcher() {
        assert_eq!(
            output_label("/tools/mirrord", &["exec", "--", "/apps/rust-sqs-printer"]),
            "rust-sqs-printer"
        );
        assert_eq!(
            output_label("/apps/kafka-consumer", &[""; 0]),
            "kafka-consumer"
        );
    }

    #[test]
    fn a_go_build_is_filed_under_its_app() {
        assert_eq!(
            output_label(
                "mirrord",
                &["exec", "--", "/src/tests/go-e2e-dir/go-e2e-dir.27"]
            ),
            "go-e2e-dir"
        );
    }
}

/// WARN messages exempted from `assert_no_warn_in_stderr` check
const ALLOWED_WARNINGS: [&str; 1] = [
    // part of the `exec` based test-harness
    "mirrord::config: Accepting invalid certificates",
];

/// Caps a [`TestProcess::wait_for_line`] call at this many times its silence budget, for a process
/// that logs steadily but never reaches the awaited line.
const MAX_SILENCE_EXTENSIONS: u32 = 5;

/// Which of a [`TestProcess`]'s captured streams a wait is watching.
#[derive(Clone, Copy)]
enum Stream {
    Stdout,
    Stderr,
}

impl std::fmt::Display for Stream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stdout => f.write_str("stdout"),
            Self::Stderr => f.write_str("stderr"),
        }
    }
}

/// Returns the last `max_bytes` of `output`, on a character boundary.
fn tail(output: &str, max_bytes: usize) -> &str {
    let Some(cut) = output.len().checked_sub(max_bytes) else {
        return output;
    };

    match output.get(cut..) {
        Some(tail) => tail,
        // `cut` landed inside a multi-byte character; step forward to the next boundary.
        None => {
            let boundary = (cut..output.len())
                .find(|&i| output.is_char_boundary(i))
                .unwrap_or(output.len());
            &output[boundary..]
        }
    }
}

/// Returns string with time format of hh:mm:ss
pub fn format_time() -> String {
    let now = Utc::now();
    format!("{:02}:{:02}:{:02}", now.hour(), now.minute(), now.second())
}

/// Wraps a bunch of things of a [`Child`] process, so we can check its output for errors/specific
/// messages.
///
/// It's mostly created by helper functions in the tests crate, where we start a child test process
/// running `mirrord`, wait for it to finish and look into its `stdout/stderr`.
pub struct TestProcess {
    /// The [`Child`] process started, running `mirrord`.
    pub child: Child,
    /// `stderr` we use to check for `ERROR` logs/messages from the test app.
    stderr_data: Arc<RwLock<String>>,
    /// `stdout` we use to check for logs/messages from the test app.
    stdout_data: Arc<RwLock<String>>,
    /// Task that reads `stderr`.
    stderr_task: Option<JoinHandle<()>>,
    /// Task that reads `stdout`.
    stdout_task: Option<JoinHandle<()>>,
    /// `^.*ERROR[^\w_-]` [`Regex`] to look for our errors in test apps.
    error_capture: Regex,
    /// `WARN` [`Regex`] to look for our warnings in test apps.
    warn_capture: Regex,
    /// Keeps the temporary directory alive for the process's lifetime.
    _tempdir: Option<TempDir>,
}

impl TestProcess {
    pub async fn get_stdout(&self) -> String {
        self.stdout_data.read().await.clone()
    }

    pub async fn get_stderr(&self) -> String {
        self.stderr_data.read().await.clone()
    }

    /// Asserts that the app logged no record of `level`, like uvicorn's `ERROR:    ...` lines, on
    /// its stderr or its stdout.
    ///
    /// It only looks for the level at the start of a line, because the output has other text with
    /// the level in it, like the CLI's log of its environment, which has the body of the bash
    /// function `rlocation` that Bazel's test setup exports, with an `echo "ERROR: ..."`.
    pub async fn assert_log_level(&self, stderr: bool, level: &str) {
        let (stream, output) = if stderr {
            ("stderr", self.stderr_data.read().await)
        } else {
            ("stdout", self.stdout_data.read().await)
        };
        let record = format!("{level}:");
        assert!(
            output
                .lines()
                .any(|line| line.trim_start().starts_with(&record))
                .not(),
            "application {stream} should not have a `{level}` record"
        );
    }

    pub async fn assert_python_fileops_stderr(&self) {
        assert!(
            self.stderr_data.read().await.contains("FAILED").not(),
            "application stderr should not contain `FAILED`"
        );
    }

    pub async fn wait_assert_success(&mut self) {
        let output = self.wait().await;
        #[cfg(not(target_os = "windows"))]
        assert!(
            output.success(),
            "application unexpectedly failed: exit code {:?}, signal code {:?}",
            output.code(),
            output.signal(),
        );

        #[cfg(target_os = "windows")]
        assert!(
            output.success(),
            "application unexpectedly failed: exit code {:?}",
            output.code(),
        );
    }

    pub async fn wait_assert_fail(&mut self) {
        let output = self.wait().await;
        #[cfg(not(target_os = "windows"))]
        assert!(
            output.success().not(),
            "application unexpectedly succeeded: exit code {:?}, signal code {:?}",
            output.code(),
            output.signal()
        );

        #[cfg(target_os = "windows")]
        assert!(
            output.success().not(),
            "application unexpectedly succeeded: exit code {:?}",
            output.code(),
        );
    }

    pub async fn assert_stdout_contains(&self, string: &str) {
        assert!(
            self.get_stdout().await.contains(string),
            "application stdout should contain `{string}`",
        );
    }

    pub async fn assert_stdout_doesnt_contain(&self, string: &str) {
        assert!(
            self.get_stdout().await.contains(string).not(),
            "application stdout should not contain `{string}`",
        );
    }

    pub async fn assert_stderr_contains(&self, string: &str) {
        assert!(
            self.get_stderr().await.contains(string),
            "application stderr should contain `{string}`",
        );
    }

    pub async fn assert_stderr_doesnt_contain(&self, string: &str) {
        assert!(
            self.get_stderr().await.contains(string).not(),
            "application stderr should not contain `{string}`",
        );
    }

    pub async fn assert_no_error_in_stdout(&self) {
        assert!(
            self.error_capture
                .is_match(&self.stdout_data.read().await)
                .unwrap()
                .not(),
            "application stdout contains an error"
        );
    }

    pub async fn assert_no_error_in_stderr(&self) {
        assert!(
            self.error_capture
                .is_match(&self.stderr_data.read().await)
                .unwrap()
                .not(),
            "application stderr contains an error"
        );
    }

    pub async fn assert_no_warn_in_stdout(&self) {
        assert!(
            self.warn_capture
                .is_match(&self.stdout_data.read().await)
                .unwrap()
                .not(),
            "application stdout contains a warning"
        );
    }

    pub async fn assert_no_warn_in_stderr(&self) {
        let stderr = &self.stderr_data.read().await;

        // exit early when no WARN lines
        if stderr
            .lines()
            .any(|line| self.warn_capture.is_match(line).unwrap())
            .not()
        {
            return;
        }

        let unexpected_warns: Vec<String> = self
            .warn_capture
            .captures_iter(stderr)
            .filter_map(|m| m.ok())
            .filter_map(|m| m.get(1).map(|m| m.as_str().trim().to_owned()))
            .filter(|warning_text| ALLOWED_WARNINGS.contains(&warning_text.as_str()).not())
            .collect();

        assert!(
            unexpected_warns.is_empty(),
            "application stderr contains unexpected warnings: {unexpected_warns:?}"
        );
    }

    /// Waits for `line` to appear in the process's `stderr`.
    ///
    /// `timeout` bounds how long the process may go *silent*, not how long the wait may take in
    /// total: every byte the process writes restarts it. Gives up after `timeout` without new
    /// output, or [`MAX_SILENCE_EXTENSIONS`] times `timeout` overall.
    pub async fn wait_for_line(&self, timeout: Duration, line: &str) {
        self.wait_for_line_in(timeout, line, Stream::Stderr).await
    }

    /// Waits for `line` to appear in the process's `stdout`.
    ///
    /// Bounds silence rather than total time, exactly as [`TestProcess::wait_for_line`] does.
    pub async fn wait_for_line_stdout(&self, timeout: Duration, line: &str) {
        self.wait_for_line_in(timeout, line, Stream::Stdout).await
    }

    fn stream_closed(&self, stream: Stream) -> bool {
        let task = match stream {
            Stream::Stderr => self.stderr_task.as_ref(),
            Stream::Stdout => self.stdout_task.as_ref(),
        };

        task.is_some_and(JoinHandle::is_finished)
    }

    async fn wait_for_line_in(&self, timeout: Duration, line: &str, stream: Stream) {
        let started = std::time::Instant::now();
        let hard_deadline = timeout.saturating_mul(MAX_SILENCE_EXTENSIONS);
        let mut last_progress = started;
        let mut seen_len = 0;

        loop {
            let output = match stream {
                Stream::Stderr => self.get_stderr().await,
                Stream::Stdout => self.get_stdout().await,
            };

            if output.contains(line) {
                return;
            }

            if output.len() > seen_len {
                seen_len = output.len();
                last_progress = std::time::Instant::now();
            }

            if self.stream_closed(stream) {
                let output = match stream {
                    Stream::Stderr => self.get_stderr().await,
                    Stream::Stdout => self.get_stdout().await,
                };

                if output.contains(line) {
                    return;
                }

                panic!(
                    "Gave up waiting for line: {line}\n\
                     {stream} closed after {:?}, so the line can no longer arrive. \
                     Last 2000 bytes:\n{}",
                    started.elapsed(),
                    tail(&output, 2000),
                );
            }

            if last_progress.elapsed() >= timeout {
                panic!(
                    "Timeout waiting for line: {line}\n\
                     {stream} produced nothing for {:?} (waited {:?} in total). Last 2000 bytes:\n{}",
                    last_progress.elapsed(),
                    started.elapsed(),
                    tail(&output, 2000),
                );
            }

            if started.elapsed() >= hard_deadline {
                panic!(
                    "Timeout waiting for line: {line}\n\
                     {stream} kept producing output but the line never arrived within {:?}. \
                     Last 2000 bytes:\n{}",
                    started.elapsed(),
                    tail(&output, 2000),
                );
            }

            // avoid busyloop
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Wait for the test app to output (at least) the given amount of lines.
    ///
    /// # Arguments
    ///
    /// * `timeout` - how long to wait for process to output enough lines.
    ///
    /// # Panics
    /// If `timeout` has passed and stdout still does not contain `n` lines.
    pub async fn await_n_lines(&self, n: usize, timeout: Duration) -> Vec<String> {
        tokio::time::timeout(timeout, async move {
            loop {
                let stdout = self.get_stdout().await;
                if stdout.lines().count() >= n {
                    return stdout
                        .lines()
                        .map(ToOwned::to_owned)
                        .collect::<Vec<String>>();
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("Test process output did not produce expected amount of lines in time.")
    }

    /// Wait for the test app to output the given amount of lines, then assert it did not output
    /// more than expected.
    ///
    /// > Note: we do not wait to make sure more lines are not printed, we just check the lines we
    /// > got after waiting for n lines. So it is possible for this to happen:
    ///   1. Test process outputs `n` lines.
    ///   2. `await_n_lines` returns those `n` lines.
    ///   3. This function asserts there are only `n` lines.
    ///   3. The test process outputs more lines.
    ///
    /// # Arguments
    ///
    /// * `timeout` - how long to wait for process to output enough lines.
    ///
    /// # Panics
    /// - If `timeout` has passed and stdout still does not contain `n` lines.
    /// - If stdout contains more than `n` lines.
    pub async fn await_exactly_n_lines(&self, n: usize, timeout: Duration) -> Vec<String> {
        let lines = self.await_n_lines(n, timeout).await;
        assert_eq!(
            lines.len(),
            n,
            "Test application printed out more lines than expected."
        );
        lines
    }

    pub async fn write_to_stdin(&mut self, data: &[u8]) {
        match self.child.stdin {
            Some(ref mut stdin) => {
                stdin.write(data).await.unwrap();
            }
            _ => {
                panic!("Can't write to test app's stdin!");
            }
        }
    }

    /// Waits for process to end and stdout/err to be read. - returns final exit status
    pub async fn wait(&mut self) -> ExitStatus {
        eprintln!("waiting for process to exit");
        let exit_status = self.child.wait().await.unwrap();
        eprintln!("process exit, waiting for stdout/err to be read");
        self.stdout_task
            .take()
            .expect("can't call wait twice")
            .await
            .unwrap();
        self.stderr_task
            .take()
            .expect("can't call wait twice")
            .await
            .unwrap();
        eprintln!("stdout/err read and finished");
        exit_status
    }

    pub fn from_child(child: Child, tempdir: Option<TempDir>) -> TestProcess {
        Self::from_child_labeled(child, tempdir, "process")
    }

    /// [`Self::from_child`] with the name the process's output is filed under, see
    /// [`LOG_DIR_ENV`].
    pub fn from_child_labeled(
        mut child: Child,
        tempdir: Option<TempDir>,
        label: &str,
    ) -> TestProcess {
        let stderr_data = Arc::new(RwLock::new(String::new()));
        let stdout_data = Arc::new(RwLock::new(String::new()));
        let child_stderr = child.stderr.take().unwrap();
        let child_stdout = child.stdout.take().unwrap();
        let stderr_data_reader = stderr_data.clone();
        let stdout_data_reader = stdout_data.clone();
        let pid = child.id().unwrap();
        let stderr_copy = output_copy(label, pid);
        let stdout_copy = stderr_copy.clone();

        let stderr_task = Some(tokio::spawn(async move {
            let mut reader = BufReader::new(child_stderr);
            let mut buf = [0; 1024];
            loop {
                let n = reader.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }

                let string = String::from_utf8_lossy(&buf[..n]);
                eprint!("stderr {} {pid}: {}", format_time(), string);
                copy_output(&stderr_copy, "stderr", &string);
                let _ = Write::flush(&mut std::io::stderr());
                {
                    stderr_data_reader.write().await.push_str(&string);
                }
            }
        }));
        let stdout_task = Some(tokio::spawn(async move {
            let mut reader = BufReader::new(child_stdout);
            let mut buf = [0; 1024];
            loop {
                let n = reader.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                let string = String::from_utf8_lossy(&buf[..n]);
                print!("stdout {} {pid}: {}", format_time(), string);
                copy_output(&stdout_copy, "stdout", &string);
                let _ = Write::flush(&mut std::io::stdout());
                {
                    stdout_data_reader.write().await.push_str(&string);
                }
            }
        }));

        let error_capture = Regex::new(r"^.*ERROR[^\w_-]").unwrap();
        let warn_capture = Regex::new(r"(?m)WARN\s+(.*)$").unwrap();

        TestProcess {
            child,
            error_capture,
            warn_capture,
            stderr_data,
            stdout_data,
            stderr_task,
            stdout_task,
            _tempdir: tempdir,
        }
    }

    pub async fn start_process(
        executable: String,
        args: Vec<String>,
        env: HashMap<String, String>,
    ) -> TestProcess {
        println!("EXECUTING: {executable}");
        let label = output_label(&executable, &args);
        let child = resolve_tokio_command(executable)
            .args(args)
            .envs(env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        println!("Started application.");
        TestProcess::from_child_labeled(child, None, &label)
    }
}

/// Ensures that the child process is killed when the `TestProcess` is dropped.
/// This is especially important on Windows, where processes may not be terminated
/// automatically when the parent process exits.
#[cfg(target_os = "windows")]
impl Drop for TestProcess {
    fn drop(&mut self) {
        // Ensure clean process termination, especially on Windows
        if let Some(pid) = self.child.id() {
            // Use Windows taskkill for more aggressive cleanup of process tree
            let _ = mirrord_command::resolve_command("taskkill")
                .args(["/F", "/T", "/PID", &pid.to_string()])
                .output();
        }
    }
}
