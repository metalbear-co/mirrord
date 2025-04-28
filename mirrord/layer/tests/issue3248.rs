#![cfg(target_os = "macos")]
#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{io::Write, os::unix::fs::PermissionsExt, path::Path, time::Duration};

use mirrord_sip::MIRRORD_TEMP_BIN_DIR_PATH_BUF;
use rstest::rstest;

mod common;

pub use common::*;

#[rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn skip_sip(dylib_path: &Path) {
    let signed_ls = sign_binary("/bin/ls");
    let signed_ls_path = signed_ls.path().to_str().unwrap();

    let signed_bash = sign_binary("/bin/bash");
    let signed_bash_path = signed_bash.path().to_str().unwrap();

    let script = create_empty_script(signed_bash_path);
    let script_path = script.path().to_str().unwrap();

    let application = Application::RustIssue3248;
    let (mut test_process, _intproxy) = application
        .start_process_with_layer(
            dylib_path,
            vec![
                ("RUST_LOG", "mirrord=trace"),
                (
                    "MIRRORD_SKIP_SIP",
                    format!("{};{}", signed_bash_path, signed_ls_path).as_str(),
                ),
                ("TEST_SCRIPT_PATH", script_path),
                ("TEST_BINARY_PATH", signed_ls_path),
            ],
            None,
        )
        .await;

    test_process.wait_assert_success().await;

    assert_not_patched(signed_ls.path());
    assert_not_patched(signed_bash.path());
}

fn sign_binary(path: &str) -> tempfile::NamedTempFile {
    let signed_temp_file = tempfile::NamedTempFile::new().unwrap();

    let mut settings = apple_codesign::SigningSettings::default();
    settings.set_code_signature_flags(
        apple_codesign::SettingsScope::Main,
        apple_codesign::CodeSignatureFlags::ADHOC | apple_codesign::CodeSignatureFlags::RESTRICT,
    );
    settings.set_binary_identifier(
        apple_codesign::SettingsScope::Main,
        signed_temp_file
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy(),
    );
    let signer = apple_codesign::UnifiedSigner::new(settings);
    signer.sign_path(path, signed_temp_file.path()).unwrap();

    signed_temp_file
}

fn create_empty_script(interpreter: &str) -> tempfile::NamedTempFile {
    let mut script = tempfile::NamedTempFile::new().unwrap();
    script
        .write_all(format!("#!{}\n", interpreter).as_bytes())
        .unwrap();
    let permissions = std::fs::Permissions::from_mode(0o700);
    std::fs::set_permissions(&script, permissions).unwrap();
    script
}

fn assert_not_patched(path: &Path) {
    let patched = MIRRORD_TEMP_BIN_DIR_PATH_BUF.join(path.strip_prefix("/").unwrap_or(path));
    assert!(!patched.exists());
}
