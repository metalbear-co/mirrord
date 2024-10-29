use std::process::exit;

fn main() {
    println!("cargo::rerun-if-env-changed=MIRRORD_LAYER_FILE_MACOS_ARM64");
    if std::env::var("MIRRORD_LAYER_FILE").is_err() {
        println!(
            "cargo:rustc-env=MIRRORD_LAYER_FILE={}",
            std::env::var("CARGO_CDYLIB_FILE_MIRRORD_LAYER").unwrap()
        );
    };

    // this check uses cargo env vars instead of conditional compilation due to cfg! not respecting
    // the target flag on a build
    if std::env::var("MIRRORD_LAYER_FILE_MACOS_ARM64").is_err()
        && std::env::var("CARGO_CFG_TARGET_ARCH").is_ok_and(|t| t.eq("aarch64"))
        && std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|t| t.eq("macos"))
    {
        println!("cargo::warning=No environment variable 'MIRRORD_LAYER_FILE_MACOS_ARM64' found - it should contain the path to the mirrord layer compiled for the `aarch64-apple-darwin` target");
        exit(1);
    };
}
