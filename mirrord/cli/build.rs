#[cfg(target_os = "windows")]
use std::process::exit;

fn recheck_and_setup_layer_file() {
    println!("cargo::rerun-if-env-changed=MIRRORD_LAYER_FILE");

    #[cfg(target_os = "macos")]
    println!("cargo::rerun-if-env-changed=MIRRORD_LAYER_FILE_MACOS_ARM64");

    if std::env::var("MIRRORD_LAYER_FILE").is_err() {
        const VAR_LAYER_FILE: &str = if cfg!(target_os = "windows") {
            "CARGO_CDYLIB_FILE_MIRRORD_LAYER_WIN"
        } else {
            "CARGO_CDYLIB_FILE_MIRRORD_LAYER"
        };
        println!(
            "cargo:rustc-env=MIRRORD_LAYER_FILE={}",
            std::env::var(VAR_LAYER_FILE).unwrap()
        );
    };
}

fn main() {
    // Make sure `MIRRORD_LAYER_FILE` is provided either by user, or computed.
    recheck_and_setup_layer_file();

    // this check uses cargo env vars instead of conditional compilation due to cfg! not respecting
    // the target flag on a build
    if std::env::var("MIRRORD_LAYER_FILE_MACOS_ARM64").is_err()
        && std::env::var("CARGO_CFG_TARGET_ARCH").is_ok_and(|t| t.eq("aarch64") || t.eq("x86_64"))
        && std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|t| t.eq("macos"))
    {
        println!(
            "cargo::warning=No environment variable 'MIRRORD_LAYER_FILE_MACOS_ARM64' found - it should contain the path to the mirrord layer compiled for the `aarch64-apple-darwin` target"
        );
        exit(1);
    };
}
