/// Links `mirrord.exe` with a 4 MiB main-thread stack.
///
/// The CLI polls its whole runtime on the main thread, and Windows gives that thread 1 MiB unless
/// the binary asks for more. A debug `mirrord exec` peaks near 1 MiB, so 4 MiB leaves room to
/// grow; `scripts/win_stack_check.ps1` holds the CLI to a tighter budget in CI, so growth shows
/// up there first.
///
/// This is set here rather than in `.cargo/config.toml` because any `RUSTFLAGS` in the environment
/// replaces the config's `rustflags` without a word, and CI toolchain setups export one by
/// default; a build like that silently gets 1 MiB.
fn link_main_thread_stack() {
    if std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|env| env == "msvc") {
        println!("cargo::rustc-link-arg-bins=/STACK:4194304");
    }
}

fn main() {
    link_main_thread_stack();
}
