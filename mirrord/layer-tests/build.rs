
fn recheck_and_setup_layer_file() {
    println!("cargo::rerun-if-env-changed=MIRRORD_LAYER_FILE");

    if std::env::var("MIRRORD_LAYER_FILE").is_err() {
        let layer_path = if cfg!(windows) {
            std::env::var("CARGO_CDYLIB_FILE_MIRRORD_LAYER_WIN").unwrap()
        } else {
            std::env::var("CARGO_CDYLIB_FILE_MIRRORD_LAYER").unwrap()
        };
        unsafe { std::env::set_var("MIRRORD_LAYER_FILE", layer_path) };
    };
    println!(
        "cargo:rustc-env=MIRRORD_LAYER_FILE={}",
        std::env::var("MIRRORD_LAYER_FILE").unwrap()
    );
}

/// By default this crate enables the `local-test-artifacts` feature, which
/// pulls in the build-dependencies for `mirrord` and the layer crates, and uses
/// `CARGO_CDYLIB_FILE_*` to populate `MIRRORD_TEST_USE_EXISTING_LIB` and
/// `MIRRORD_TESTS_USE_BINARY` from the artifacts' computed path.
/// In CI you can disable `local-test-artifacts` and 
/// provide the env vars to avoid costly redundant rebuilds.
fn main() {
    recheck_and_setup_layer_file();
    println!(
        "cargo:rustc-env=MIRRORD_TEST_USE_EXISTING_LIB={}",
        std::env::var("MIRRORD_LAYER_FILE").unwrap()
    );
    println!(
        "cargo:rustc-env=MIRRORD_TESTS_USE_BINARY={}",
        std::env::var("CARGO_BIN_FILE_MIRRORD").unwrap()
    );
}
