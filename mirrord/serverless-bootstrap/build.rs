fn should_skip_build_req() -> bool {
    std::env::var("RUSTDOCFLAGS").is_ok() || std::env::var("CLIPPY_ARGS").is_ok()
}

fn main() {
    println!("cargo:rerun-if-env-changed=MIRRORD_AGENT_BINARY");

    let agent_binary = if should_skip_build_req() {
        // use bs value
        std::env::var("CARGO_MANIFEST_PATH").unwrap()
    } else {
        std::env::var("MIRRORD_AGENT_BINARY")
            .expect("MIRRORD_AGENT_BINARY must point at the built mirrord-agent binary")
    };

    println!("cargo:rerun-if-changed={agent_binary}");
    println!("cargo:rustc-env=MIRRORD_AGENT_BINARY={agent_binary}");
}
