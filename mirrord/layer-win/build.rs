fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // Detours-style import injection resolves the payload through ordinal 1.
        println!("cargo:rustc-cdylib-link-arg=/EXPORT:mirrord_stork_marker,@1");
    }
}
