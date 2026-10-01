use std::{path::Path, process::Command};

use mirrord_sip::{
    APPLE_UTILS_VERSION, MIRRORD_BINARIES_DIR_PATH_BUF, SipPatchOptions, extract_sip_binaries,
    sip_patch,
};
use prettytable::{Table, row};
use serde::Serialize;

use crate::{CliResult, config::DiagnoseSipFormat};

fn command_output(command: &mut Command) -> Option<String> {
    command.output().ok().and_then(|output| {
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    })
}

/// A report with all the details of a binary that was missing from the `appleutils` bundle and used
/// Rosetta as a fallback. Users can generate these when warned about missing binaries and send us
/// the info so we can add the binary to metalbear-co/appleutils.
///
/// Can be printed as a table or JSON.
#[derive(Serialize)]
struct SipDiagnosticReport {
    mirrord_version: &'static str,
    macos_version: Option<String>,
    cli_architecture: &'static str,
    appleutils_version: &'static str,
    protected_binary: String,
    file: Option<String>,
    result: &'static str,
}

/// Command to check if the given binary is currently relying on Rosetta (which reaches EOL in macOS
/// 27) and if it does, prints a [`SipDiagnosticReport`] that we can use to add it to our bundled
/// utils.
pub(crate) fn diagnose_sip(binary: &Path, format: DiagnoseSipFormat) -> CliResult<()> {
    extract_sip_binaries(
        &MIRRORD_BINARIES_DIR_PATH_BUF,
        crate::execution::COMPRESSED_SIP_BINARIES,
    )?;

    let binary = binary.to_string_lossy();
    let result = sip_patch(
        &binary,
        SipPatchOptions {
            sip_binaries_dir: Some(MIRRORD_BINARIES_DIR_PATH_BUF.as_path()),
            ..Default::default()
        },
        None,
    )?;
    let Some(fallback) = result.and_then(|result| result.x64_fallback) else {
        match format {
            DiagnoseSipFormat::Table => {
                println!("This binary does not use the Rosetta fallback.")
            }
            DiagnoseSipFormat::Json => println!("{{}}"),
        }
        return Ok(());
    };

    let report = SipDiagnosticReport {
        mirrord_version: env!("CARGO_PKG_VERSION"),
        macos_version: command_output(Command::new("sw_vers").arg("-productVersion")),
        cli_architecture: std::env::consts::ARCH,
        appleutils_version: APPLE_UTILS_VERSION,
        file: command_output(Command::new("/usr/bin/file").arg(&fallback).arg("-b"))
            .map(|s| s.replace('\t', " ")),
        protected_binary: fallback.display().to_string(),
        result: "missing from appleutils; x86_64 fallback required",
    };

    match format {
        DiagnoseSipFormat::Table => {
            let mut table = Table::new();
            table.add_row(row!["FIELD", "VALUE"]);
            table.add_row(row!["mirrord version", report.mirrord_version]);
            table.add_row(row![
                "macOS version",
                report.macos_version.as_deref().unwrap_or("unavailable")
            ]);
            table.add_row(row!["CLI architecture", report.cli_architecture]);
            table.add_row(row!["appleutils version", report.appleutils_version]);
            table.add_row(row!["protected binary", report.protected_binary]);
            table.add_row(row![
                "file",
                report.file.as_deref().unwrap_or("unavailable")
            ]);
            table.add_row(row!["result", report.result]);
            table.printstd();
        }
        DiagnoseSipFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
    }
    Ok(())
}
