#![cfg(target_os = "macos")]

use std::{fs, process::Command};

mod common;

pub use common::*;

/// Regression test for `execve` reading an environment that the layer itself freed.
///
/// The app execs a hardened copy of itself from inside a `.app` bundle, passing `environ` along.
/// Patching that copy for SIP sets `DYLD_FALLBACK_FRAMEWORK_PATH`, which moves `environ` and frees
/// the array the app passed. The hook used to read the environment after that, and crashed or
/// handed the new process garbage.
///
/// Ignored by default: CI runners have SIP disabled, where the test proves nothing. Run it locally
/// with `--run-ignored all`.
#[ignore = "only meaningful with SIP enabled, which CI runners do not have"]
#[tokio::test]
async fn exec_with_environment_freed_by_sip_patching() {
    let bundle = tempfile::tempdir().unwrap();
    let macos_dir = bundle.path().join("Test.app/Contents/MacOS");
    fs::create_dir_all(&macos_dir).unwrap();
    let binary = macos_dir.join("exec_freed_env");
    fs::copy("tests/apps/exec_freed_env/out.c_test_app", &binary).unwrap();

    // The hardened runtime makes the layer patch the copy for SIP.
    #[allow(
        clippy::disallowed_methods,
        reason = "`codesign` is a macOS executable, which the Windows workaround does not apply to"
    )]
    let signed = Command::new("codesign")
        .args(["--force", "--sign", "-", "--options", "runtime"])
        .arg(&binary)
        .status()
        .unwrap();
    assert!(signed.success());

    let application = Application::DynamicApp(
        "tests/apps/exec_freed_env/out.c_test_app".to_owned(),
        vec![binary.to_str().unwrap().to_owned()],
    );
    let (mut test_process, _intproxy) = application
        .start_process(vec![("MIRRORD_FILE_MODE", "local")], None)
        .await;

    test_process.wait_assert_success().await;
    test_process
        .assert_stdout_contains("MIRRORD_EXEC_FREED_ENV=intact")
        .await;
    test_process.assert_no_error_in_stderr().await;
}
