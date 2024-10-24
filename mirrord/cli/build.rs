fn main() {
    if std::env::var("MIRRORD_LAYER_FILE").is_err() {
        println!(
            "cargo:rustc-env=MIRRORD_LAYER_FILE={}",
            std::env::var("CARGO_CDYLIB_FILE_MIRRORD_LAYER").unwrap()
        );
    };

    // TODO: is it better to fail here or pass in cargo layer file?
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    if std::env::var("MIRRORD_MACOS_ARM64_LIBRARY").is_err() {
        println!(
            "cargo:rustc-env=MIRRORD_MACOS_ARM64_LIBRARY={}",
            std::env::var("CARGO_CDYLIB_FILE_MIRRORD_LAYER").unwrap()
        );
    };
}
