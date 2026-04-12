use std::process::exit;

fn main() {
    if std::env::var("CLIPPY_ARGS").is_err() {
        return;
    }

    let manifest_path = match std::env::var("CARGO_MANIFEST_PATH") {
        Ok(path) => path,
        Err(_) => exit(1),
    };

    println!("cargo:rustc-env=MIRRORD_LAYER_FILE={manifest_path}");
    println!("cargo:rustc-env=MIRRORD_LAYER_FILE_MACOS_ARM64={manifest_path}");
    println!("cargo:rustc-env=MIRRORD_WIZARD_TAR={manifest_path}");
    println!("cargo:rustc-env=MIRRORD_SIP_BINARIES_TAR={manifest_path}");
}
