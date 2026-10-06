// Cargo reads a build script's directives from its stdout, so `println!` is the interface here
// rather than stray console output.
#![allow(clippy::disallowed_macros)]

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    // Import-table injection by stork (https://github.com/metalbear-co/stork) resolves the payload
    // through ordinal 1, the same contract as Detours'
    // `DetourCreateProcessWithDllEx` (https://github.com/microsoft/detours/wiki/DetourCreateProcessWithDllEx),
    // which inspired it. `/EXPORT:..,@N` is MSVC linker syntax, and a GNU linker rejects it, so
    // only MSVC builds get the export.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-cdylib-link-arg=/EXPORT:mirrord_stork_marker,@1");
    } else {
        println!(
            "cargo:warning=mirrord-layer-win: IAT injection is unsupported on this target, \
            because only the MSVC linker exports `mirrord_stork_marker` at ordinal 1"
        );
    }
}
