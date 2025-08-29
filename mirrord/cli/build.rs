#[cfg(target_os = "windows")]
use std::path::Path;
use std::process::exit;

#[cfg(target_os = "macos")]
fn recheck_and_setup_layer_file() {
    println!("cargo::rerun-if-env-changed=MIRRORD_LAYER_FILE_MACOS_ARM64");
    if std::env::var("MIRRORD_LAYER_FILE").is_err() {
        println!(
            "cargo:rustc-env=MIRRORD_LAYER_FILE={}",
            std::env::var("CARGO_CDYLIB_FILE_MIRRORD_LAYER").unwrap()
        );
    };
}

#[cfg(target_os = "windows")]
fn recheck_and_setup_layer_file() {
    println!("cargo::rerun-if-env-changed=MIRRORD_LAYER_FILE");
    if std::env::var("MIRRORD_LAYER_FILE").is_err() {
        let out_dir_env =
            std::env::var("OUT_DIR").expect("Failed getting OUT_DIR from environment");
        let out_dir = Path::new(&out_dir_env);
        let build_dir = out_dir
            .ancestors()
            .nth(3)
            .expect("Failed extracting build directory from OUT_DIR");
        let layer = build_dir.join("layer_win.dll");

        println!(
            "cargo:rustc-env=MIRRORD_LAYER_FILE={}",
            layer.to_str().unwrap()
        );
    };
}

fn main() {
    // Make sure `MIRRORD_LAYER_FILE` is provided either by user, or computed.
    #[cfg(not(target_os = "linux"))]
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
