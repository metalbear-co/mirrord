use std::{
    fmt::{Display, Write},
    path::Path,
};

use apple_codesign::{CodeSignatureFlags, SettingsScope, SigningSettings, UnifiedSigner};
use rand::Rng;

use crate::error::Result;

const EMPTY_ENTITLEMENTS_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict></dict></plist>"#;

/// N is the length of the string to generate. If N is odd, the length will be N-1.
fn generate_hex_string<const N: usize>() -> String {
    let mut rng = rand::rng();
    (0..N / 2) // N/2 bytes = N hexadecimal digits.
        .fold(String::new(), |mut acc, _next_in_range| {
            // ignoring result: writing to String can't fail.
            let _ = write!(acc, "{:02x}", rng.random::<u8>());
            acc
        })
}

pub(crate) fn sign<PI: AsRef<Path>, PO: AsRef<Path>, PR: Display>(
    input: PI,
    output: PO,
    bin_id_prefix: Option<PR>,
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
        bin_id_prefix.map_or_else(
            || format!("mirrord-patched-bin-{}", generate_hex_string::<40>()),
            |prefix| format!("{}-{}", prefix, generate_hex_string::<40>()),
        ),
    );

    // Set an empty entitlements XML, as the default settings leave the existing entitlements set.
    settings.set_entitlements_xml(SettingsScope::Main, EMPTY_ENTITLEMENTS_PLIST)?;

    let signer = UnifiedSigner::new(settings);
    signer.sign_path(input, output)?;
    Ok(())
}
