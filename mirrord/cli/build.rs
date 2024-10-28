#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use std::process::exit;

fn main() {
    if std::env::var("MIRRORD_LAYER_FILE").is_err() {
        println!(
            "cargo:rustc-env=MIRRORD_LAYER_FILE={}",
            std::env::var("CARGO_CDYLIB_FILE_MIRRORD_LAYER").unwrap()
        );
    };

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    if std::env::var("MIRRORD_LAYER_FILE_MACOS_ARM64").is_err() {
        println!("cargo::warning=No environment variable 'MIRRORD_LAYER_FILE_MACOS_ARM64' found - it should contain the path to the mirrord layer compiled for the `aarch64-apple-darwin` target");
        exit(1);
    };
}
