compile/test speed improvements

1. add CARGO_NET_GIT_FETCH_WITH_CLI=true to agent's Dockerfile since we found out it
    saves a lot of time on fetching (around takes 60s when using libgit2)
2. change `rust-toolchain.toml` so it won't auto install unneeded targets always
3. remove `toolchain: nightly` parameter from `actions-rs/toolchain@v1` since it's
    not needed because we have `rust-toolchain.toml`
    saves a lot of time on fetching (takes around 60s when using libgit2)
4. switch to use `actions-rust-lang/setup-rust-toolchain@v1` instead of `actions-rs/toolchain@v1` 
    since it's deprecated and doesn't support `rust-toolchain.toml`
5. remove s`Swatinem/rust-cache@v2` since it's included in `actions-rust-lang/setup-rust-toolchain@v1`
6. use latest version of `Apple-Actions/import-codesign-certs` to remove warnings