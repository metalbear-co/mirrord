compile/test speed improvements

1. add CARGO_NET_GIT_FETCH_WITH_CLI=true to agent's Dockerfile since we found out it
    saves a lot of time on fetching (takes around 60s when using libgit2)
2. change `rust-toolchain.toml` so it won't auto install unneeded targets always