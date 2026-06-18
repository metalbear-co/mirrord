use std::{
    ffi::OsStr,
    io,
    ops::Not,
    path::{Path, PathBuf},
    process::{Command, Output},
    thread,
    time::Duration,
};

use apple_codesign::{CodeSignatureFlags, SettingsScope, SigningSettings, UnifiedSigner};
use rand::RngCore;

use crate::{
    error::{Result, SipError},
    logger::SipLoggerGuard,
};

const EMPTY_ENTITLEMENTS_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict></dict></plist>"#;

/// mirrord auto detects santa (or user can set it) and changes codesign behavior
/// see sip logic for more info.
pub const MIRRORD_SANTA_MODE_ENV: &str = "MIRRORD_SANTA_MODE";

fn get_santa_path() -> PathBuf {
    which::which("santactl").unwrap_or_else(|_| Path::new("/usr/local/bin/santactl").to_path_buf())
}

/// Hex string from random 20 bytes (results in 40 hexadecimal digits).
fn generate_hex_string() -> String {
    let mut rng = rand::rng();
    let mut bytes = [0u8; 20];
    rng.fill_bytes(&mut bytes[..]);
    hex::encode(bytes)
}

pub(crate) fn sign(input: &Path, original: &Path, logger: &mut SipLoggerGuard<'_>) -> Result<()> {
    if use_codesign_binary() {
        logger.log("Using codesign binary");
        sign_with_codesign_binary(input, logger)
    } else {
        logger.log("Using apple codesign binary");
        sign_with_apple_codesign(input, original)
    }
}

fn sign_with_apple_codesign(input: &Path, original: &Path) -> Result<()> {
    // In the past, we used the codesign binary
    // but we had an issue where it received EBADF (bad file descriptor)
    // probably since in some flows, like Go,
    // it calls fork then execve, then we do the same from our code (to call codesign).
    // We switched to apple codesign crate to avoid creating a child process.
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
                .file_name()
                .map(OsStr::to_string_lossy)
                .unwrap_or_else(|| "mirrord-patched-bin".into()),
            generate_hex_string()
        ),
    );

    // Set an empty entitlements XML, as the default settings leave the existing entitlements set.
    settings.set_entitlements_xml(SettingsScope::Main, EMPTY_ENTITLEMENTS_PLIST)?;

    let signer = UnifiedSigner::new(settings);
    signer.sign_path_in_place(input)?;
    Ok(())
}

fn sign_with_codesign_binary(binary: &Path, logger: &mut SipLoggerGuard<'_>) -> Result<()> {
    let output = Command::new("/usr/bin/codesign")
        .arg("-s") // sign with identity
        .arg("-") // adhoc identity
        .arg("-f") // force (might have a signature already)
        .arg(binary)
        .env_clear()
        .output()?;

    if output.status.success().not() {
        return Err(SipError::Sign(
            output.status,
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    // Allow Santa some time to observe the new signature.
    // https://northpole.dev/features/binary-authorization/#allowlist-compiler
    // > While Santa tries to ensure all files created by allowlisted compilers are scanned
    // > and transitive rules created as quickly as possible,
    // > there is a race condition in certain scenarios that will cause execution to fail,
    // > especially if a binary is executed immediately after being created.
    thread::sleep(Duration::from_millis(100));

    if logger.enabled() {
        let mut command = Command::new(get_santa_path());
        command.arg("fileinfo").arg(binary);
        let output = command.output();
        log_command_result(logger, command, output);

        let mut command = Command::new("/usr/bin/codesign");
        command.arg("-dvv").arg(binary);
        let output = command.output();
        log_command_result(logger, command, output);
    }

    Ok(())
}

fn log_command_result(
    logger: &mut SipLoggerGuard<'_>,
    command: Command,
    output: io::Result<Output>,
) {
    match output {
        Ok(output) => {
            logger.log(format_args!(
                "Command {command:?} finished with status {}.\nSTDERR={}\nSTDOUT={}",
                output.status,
                String::from_utf8_lossy(&output.stderr),
                String::from_utf8_lossy(&output.stdout),
            ));
        }
        Err(error) => {
            logger.log(format_args!(
                "Failed to execute command {command:?}: {error:?}"
            ));
        }
    }
}

/// macOS's codesign binary is usually set as a "Compiler Rule"
/// and has transistive allowance - meaning any binary it creates is also allowed to run
/// so if we use the systems' codesign, we can bypass Santa's block.
fn use_codesign_binary() -> bool {
    std::env::var(MIRRORD_SANTA_MODE_ENV)
        .ok()
        .and_then(|value| value.trim().to_ascii_lowercase().parse::<bool>().ok())
        .unwrap_or(false)
}
