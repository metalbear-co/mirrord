fn should_skip_binary_embedding() -> bool {
    std::env::var("RUSTDOCFLAGS").is_ok()
        || std::env::var("CLIPPY_ARGS").is_ok()
        || std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "linux"
}

fn binary_env(var_name: &str) -> String {
    if should_skip_binary_embedding() {
        // in builds that dont require the actual embedded binaries
        // use cargo manifest file path as dummy path that always exists
        std::env::var("CARGO_MANIFEST_PATH").unwrap()
    } else {
        std::env::var(var_name)
            .unwrap_or_else(|_| panic!("{var_name} must point at the built artifact"))
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=MIRRORD_AGENT_BINARY");
    println!("cargo:rerun-if-env-changed=MIRRORD_REMOTE_LAYER_BINARY");

    let agent_binary = binary_env("MIRRORD_AGENT_BINARY");
    let remote_layer_binary = binary_env("MIRRORD_REMOTE_LAYER_BINARY");

    println!("cargo:rerun-if-changed={agent_binary}");
    println!("cargo:rerun-if-changed={remote_layer_binary}");
    println!("cargo:rustc-env=MIRRORD_AGENT_BINARY={agent_binary}");
    println!("cargo:rustc-env=MIRRORD_REMOTE_LAYER_BINARY={remote_layer_binary}");
}
