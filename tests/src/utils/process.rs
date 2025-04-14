use core::ops::Not;
use std::{
    collections::HashMap,
    os::unix::process::ExitStatusExt,
    process::{ExitStatus, Stdio},
    sync::Arc,
    time::Duration,
};

use fancy_regex::Regex;
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
    sync::RwLock,
    task::JoinHandle,
};

use crate::utils::format_time;

pub struct TestProcess {
    pub child: Child,
    stderr_data: Arc<RwLock<String>>,
    stdout_data: Arc<RwLock<String>>,
    stderr_task: Option<JoinHandle<()>>,
    stdout_task: Option<JoinHandle<()>>,
    error_capture: Regex,
    warn_capture: Regex,
    // Keeps tempdir existing while process is running.
    _tempdir: Option<TempDir>,
}

impl TestProcess {
    pub async fn get_stdout(&self) -> String {
        self.stdout_data.read().await.clone()
    }

    pub async fn get_stderr(&self) -> String {
        self.stderr_data.read().await.clone()
    }

    pub async fn assert_log_level(&self, stderr: bool, level: &str) {
        if stderr {
            assert!(
                self.stderr_data.read().await.contains(level).not(),
                "application stderr should not contain `{level}`"
            );
        } else {
            assert!(
                self.stdout_data.read().await.contains(level).not(),
                "application stdout should not contain `{level}`"
            );
        }
    }

    pub async fn assert_python_fileops_stderr(&self) {
        assert!(
            self.stderr_data.read().await.contains("FAILED").not(),
            "application stderr should not contain `FAILED`"
        );
    }

    pub async fn wait_assert_success(&mut self) {
        let output = self.wait().await;
        assert!(
            output.success(),
            "application unexpectedly failed: exit code {:?}, signal code {:?}",
            output.code(),
            output.signal(),
        );
    }

    pub async fn wait_assert_fail(&mut self) {
        let output = self.wait().await;
        assert!(
            output.success().not(),
            "application unexpectedly succeeded: exit code {:?}, signal code {:?}",
            output.code(),
            output.signal()
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
        assert!(
            self.warn_capture
                .is_match(&self.stderr_data.read().await)
                .unwrap()
                .not(),
            "application stderr contains a warning"
        );
    }

    pub async fn wait_for_line(&self, timeout: Duration, line: &str) {
        let now = std::time::Instant::now();
        while now.elapsed() < timeout {
            let stderr = self.get_stderr().await;
            if stderr.contains(line) {
                return;
            }
            // avoid busyloop
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("Timeout waiting for line: {line}");
    }

    pub async fn wait_for_line_stdout(&self, timeout: Duration, line: &str) {
        let now = std::time::Instant::now();
        while now.elapsed() < timeout {
            let stdout = self.get_stdout().await;
            if stdout.contains(line) {
                return;
            }
            // avoid busyloop
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("Timeout waiting for line: {line}");
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
                        .map(ToString::to_string)
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
        if let Some(ref mut stdin) = self.child.stdin {
            stdin.write(data).await.unwrap();
        } else {
            panic!("Can't write to test app's stdin!");
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

    pub fn from_child(mut child: Child, tempdir: Option<TempDir>) -> TestProcess {
        let stderr_data = Arc::new(RwLock::new(String::new()));
        let stdout_data = Arc::new(RwLock::new(String::new()));
        let child_stderr = child.stderr.take().unwrap();
        let child_stdout = child.stdout.take().unwrap();
        let stderr_data_reader = stderr_data.clone();
        let stdout_data_reader = stdout_data.clone();
        let pid = child.id().unwrap();

        let stderr_task = Some(tokio::spawn(async move {
            let mut reader = BufReader::new(child_stderr);
            let mut buf = [0; 1024];
            loop {
                let n = reader.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }

                let string = String::from_utf8_lossy(&buf[..n]);
                eprintln!("stderr {} {pid}: {}", format_time(), string);
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
                {
                    stdout_data_reader.write().await.push_str(&string);
                }
            }
        }));

        let error_capture = Regex::new(r"^.*ERROR[^\w_-]").unwrap();
        let warn_capture = Regex::new(r"WARN").unwrap();

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
        let child = Command::new(executable)
            .args(args)
            .envs(env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        println!("Started application.");
        TestProcess::from_child(child, None)
    }
}
