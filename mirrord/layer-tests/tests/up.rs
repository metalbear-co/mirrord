//! Runs `mirrord up` itself, against the test intproxy instead of a cluster.
//!
//! Every service is a `mirrord exec` child of `mirrord up`, and all of them connect to the same
//! [`TestIntProxy`]: the children inherit `MIRRORD_TEST_INTPROXY_ADDR`, and `mirrord up` never
//! talks to the cluster when every service names its target.

#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::{collections::HashMap, fs, ops::Not, path::Path, time::Duration};

use mirrord_config::MIRRORD_TEST_INTPROXY_ADDR;
use mirrord_test_utils::run_command::run_mirrord;
#[cfg(unix)]
use nix::{sys::signal::kill, unistd::Pid};
use tempfile::TempDir;
use tokio::net::TcpListener;
#[cfg(windows)]
use winapi::{
    shared::{minwindef::FALSE, winerror::WAIT_TIMEOUT},
    um::{processthreadsapi::OpenProcess, synchapi::WaitForSingleObject, winnt::SYNCHRONIZE},
};

mod common;
pub use common::*;

/// One `services` entry of a `mirrord-up.yaml` that runs `script` with python.
///
/// Files stay local and DNS is not remote, as the test intproxy answers neither.
fn service(python: &str, name: &str, script: &str) -> String {
    let command = serde_json::to_string(&[python, "-c", script]).unwrap();
    format!(
        r#"
  {name}:
    target:
      path: pod/mock-target
    default_mode: mirror
    config_patch:
      feature:
        fs:
          mode: local
        network:
          dns: false
    run:
      command: {command}
"#
    )
}

/// Runs `mirrord up` on a `mirrord-up.yaml` in `directory` with the given `services` entries.
async fn run_up(directory: &Path, services: &[String]) -> (TestProcess, TestIntProxy) {
    let config = directory.join("mirrord-up.yaml");
    fs::write(
        &config,
        format!(
            "common:\n  telemetry: false\n  operator: false\nservices:{}",
            services.concat()
        ),
    )
    .unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let env = HashMap::from([
        (MIRRORD_TEST_INTPROXY_ADDR, address.as_str()),
        ("MIRRORD_CHECK_VERSION", "false"),
    ]);
    let process = run_mirrord(
        vec![
            "up",
            "-f",
            config.to_str().unwrap(),
            "-k",
            "test",
            "--no-ui",
        ],
        env,
        None,
    )
    .await;

    (process, TestIntProxy::new(listener, None).await)
}

/// A python string literal holding `path`. JSON escapes it the way python reads it.
fn python_path(path: &Path) -> String {
    serde_json::to_string(path).unwrap()
}

/// A service's process, opened while it is known to be running.
///
/// On Windows the open handle keeps the PID from being reused, so a check after shutdown cannot
/// mistake another process for the service. Unix does not hand out a PID again this soon.
#[cfg(windows)]
struct Process(OwnedHandle);

#[cfg(windows)]
impl Process {
    fn open(pid: u32) -> Self {
        let handle = unsafe { OpenProcess(SYNCHRONIZE, FALSE, pid) };
        assert!(
            handle.is_null().not(),
            "failed to open process {pid}: {}",
            std::io::Error::last_os_error()
        );
        Self(unsafe { OwnedHandle::from_raw_handle(handle.cast()) })
    }

    fn is_running(&self) -> bool {
        unsafe { WaitForSingleObject(self.0.as_raw_handle().cast(), 0) == WAIT_TIMEOUT }
    }
}

#[cfg(unix)]
struct Process(Pid);

#[cfg(unix)]
impl Process {
    fn open(pid: u32) -> Self {
        Self(Pid::from_raw(pid as i32))
    }

    fn is_running(&self) -> bool {
        kill(self.0, None).is_ok()
    }
}

#[tokio::test]
async fn up_prefixes_service_output_and_ends_with_the_service() {
    let directory = TempDir::new().unwrap();
    let python = Application::get_python3_executable().await;
    let (mut up, _intproxy) = run_up(
        directory.path(),
        &[service(
            &python,
            "greeter",
            "print('hello from mirrord up')",
        )],
    )
    .await;

    tokio::time::timeout(Duration::from_secs(30), up.wait_assert_success())
        .await
        .expect("mirrord up did not end with its service");
    up.assert_stdout_contains("greeter: Ready!").await;
    up.assert_stdout_contains("greeter: hello from mirrord up")
        .await;
}

#[tokio::test]
async fn up_reports_a_crashed_service_and_stops_the_others() {
    let directory = TempDir::new().unwrap();
    let python = Application::get_python3_executable().await;
    let pid_file = directory.path().join("sleeper.pid");
    let partial = directory.path().join("sleeper.partial");
    let release = directory.path().join("release");
    let sleeper = format!(
        "import os, time\n\
         open({partial}, 'w').write(str(os.getpid()))\n\
         os.replace({partial}, {pid_file})\n\
         time.sleep(60)",
        partial = python_path(&partial),
        pid_file = python_path(&pid_file),
    );
    let failing = format!(
        "import os, sys, time\n\
         deadline = time.time() + 60\n\
         while not os.path.exists({release}) and time.time() < deadline: time.sleep(0.02)\n\
         sys.exit(3)",
        release = python_path(&release),
    );
    let (mut up, _intproxy) = run_up(
        directory.path(),
        &[
            service(&python, "sleeper", &sleeper),
            service(&python, "failing", &failing),
        ],
    )
    .await;

    // The other service crashes only once the sleeper is open here, so `mirrord up` has a
    // running service to stop and the test can tell whether it did.
    tokio::time::timeout(Duration::from_secs(30), async {
        while pid_file.exists().not() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the sleeper service never started");
    let sleeper = Process::open(fs::read_to_string(&pid_file).unwrap().parse().unwrap());
    fs::write(&release, []).unwrap();

    tokio::time::timeout(Duration::from_secs(30), up.wait_assert_fail())
        .await
        .expect("mirrord up did not stop the remaining service");
    up.assert_stderr_contains("Service failing crashed").await;
    assert!(
        sleeper.is_running().not(),
        "the sleeper service must be stopped with the session"
    );
}
