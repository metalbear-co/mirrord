fn main() {
    println!("cargo:rerun-if-env-changed=MIRRORD_AGENT_BINARY");

    let agent_binary = std::env::var("MIRRORD_AGENT_BINARY")
        .expect("MIRRORD_AGENT_BINARY must point at the built mirrord-agent binary");

    println!("cargo:rerun-if-changed={agent_binary}");
    println!("cargo:rustc-env=MIRRORD_AGENT_BINARY={agent_binary}");
}
