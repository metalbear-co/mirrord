fn main() {
    if std::env::var("MIRRORD_LAYER_FILE").is_err() {
        println!(
            "cargo:rustc-env=MIRRORD_LAYER_FILE={}",
            std::env::var("CARGO_CDYLIB_FILE_MIRRORD_LAYER").unwrap()
        );
    };
}
