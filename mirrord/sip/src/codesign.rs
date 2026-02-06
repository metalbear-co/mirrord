use std::{
    ffi::OsStr, os::unix::process::ExitStatusExt, path::Path, process::Command, thread,
    time::Duration,
};

use apple_codesign::{CodeSignatureFlags, SettingsScope, SigningSettings, UnifiedSigner};
use rand::RngCore;

use crate::error::{Result, SipError};

const EMPTY_ENTITLEMENTS_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict></dict></plist>"#;

/// Hex string from random 20 bytes (results in 40 hexadecimal digits).
fn generate_hex_string() -> String {
    let mut rng = rand::rng();
    let mut bytes = [0u8; 20];
    rng.fill_bytes(&mut bytes[..]);
    hex::encode(bytes)
}

pub(crate) fn sign<PI: AsRef<Path>, PO: AsRef<Path>, PN: AsRef<Path>>(
    input: PI,
    output: PO,
    original: PN,
) -> Result<()> {
    if use_codesign_binary() {
        tracing::debug!("using codesign binary");
        sign_with_codesign_binary(input, output)
    } else {
        sign_with_apple_codesign(input, output, original)
    }
}

fn sign_with_apple_codesign<PI: AsRef<Path>, PO: AsRef<Path>, PN: AsRef<Path>>(
    input: PI,
    output: PO,
    original: PN,
) -> Result<()> {
    // in the past, we used the codesign binary
    // but we had an issue where it received EBADF (bad file descriptor)
    // probably since in some flows, like Go,
    // it calls fork then execve, then we do the same from our code (to call codesign)
    // we switched to use apple codesign crate to avoid this issue (of creating process)
    // but if we sign in place we get permission error, probably because someone is holding the
    // handle to the named temp file.
    let mut settings = SigningSettings::default();

    // Replace any existing flags with just the adhoc flag.
    // Important because some binaries (e.g. `go`), have the "runtime" flag set, which means,
    // opting into the hardened runtime, which strips away DYLD_INSERT_LIBRARIES etc.
    settings.set_code_signature_flags(SettingsScope::Main, CodeSignatureFlags::ADHOC);

    // `man codesign`:
    // > It is a **very bad idea** to sign different programs with the same identifier.
    settings.set_binary_identifier(
        SettingsScope::Main,
        format!(
            "{}-{}",
            original
                .as_ref()
                .file_name()
                .map(OsStr::to_string_lossy)
                .unwrap_or_else(|| "mirrord-patched-bin".into()),
            generate_hex_string()
        ),
    );

    // Set an empty entitlements XML, as the default settings leave the existing entitlements set.
    settings.set_entitlements_xml(SettingsScope::Main, EMPTY_ENTITLEMENTS_PLIST)?;

    let signer = UnifiedSigner::new(settings);
    signer.sign_path(input, output)?;
    Ok(())
}

fn sign_with_codesign_binary<PI: AsRef<Path>, PO: AsRef<Path>>(
    input: PI,
    output: PO,
) -> Result<()> {
    std::fs::copy(input.as_ref(), output.as_ref())?;
    let output_status = Command::new("/usr/bin/codesign")
        .arg("-s") // sign with identity
        .arg("-") // adhoc identity
        .arg("-f") // force (might have a signature already)
        .arg(output.as_ref())
        .env_clear()
        .output()?;

    // Allow Santa some time to observe the new signature.
    // https://northpole.dev/features/binary-authorization/#allowlist-compiler
    // > While Santa tries to ensure all files created by allowlisted compilers are scanned
    // > and transitive rules created as quickly as possible,
    // > there is a race condition in certain scenarios that will cause execution to fail,
    // > especially if a binary is executed immediately after being created.
    thread::sleep(Duration::from_millis(100));

    if output_status.status.success() {
        Ok(())
    } else {
        Err(SipError::Sign(
            output_status.status,
            String::from_utf8_lossy(&output_status.stderr).to_string(),
        ))
    }
}

fn use_codesign_binary() -> bool {
    std::env::var("MIRRORD_SANTA_MODE")
        .ok()
        .and_then(|value| value.trim().to_ascii_lowercase().parse::<bool>().ok())
        .unwrap_or(false)
}
