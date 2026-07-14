fn should_skip_build_req() -> bool {
    std::env::var("RUSTDOCFLAGS").is_ok() || std::env::var("CLIPPY_ARGS").is_ok()
}

fn binary_env(var_name: &str) -> String {
    if should_skip_build_req() {
        // use bs value
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
