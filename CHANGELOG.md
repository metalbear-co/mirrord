# Change Log

All notable changes to the mirrord's cli, agent, protocol, extensions will be documented in this file.
Previous versions had CHANGELOG per component, we decided to combine all repositories to a mono-repo with one CHANGELOG.

Check [Keep a Changelog](http://keepachangelog.com/) for recommendations on how to structure this file.

## [Unreleased]

### Fixed
- mirrord-layer: Go environment variables crash - run Go env setup in a different stack (should fix [#292](https://github.com/metalbear-co/mirrord/issues/292))

### Changed
- mirrord-layer: Add `#![feature(let_chains)]` to `lib.rs` to support new compiler version.

## 2.10.1
### Fixed
- CI:Release - Fix typo that broke the build

## 2.10.0
### Added
- New feature, [tcp outgoing traffic](https://github.com/metalbear-co/mirrord/issues/27). It's now possible to make requests to a remote host from the staging environment context. You can enable this feature setting the `MIRRORD_TCP_OUTGOING` variable to true, or using the `-o` option in mirrord-cli.
- mirrord-cli add login command for logging in to metalbear-cloud
- CI:Release - Provide zip and sha256 sums

### Fixed
- Environment variables feature on Golang programs. Issue #292 closed in #299

## 2.9.1
### Fixed
- CI - set typescript version at 4.7.4 to fix broken release action

## 2.9.0
### Added
- Support for Golang fileops
- IntelliJ Extension for mirrord

### Changed
- mirrord-layer: Added common `Result` type to to reduce boilerplate, removed dependency of `anyhow` crate.
- mirrord-layer: Split `LayerError` into `LayerError` and `HookError` to distinguish between errors that can be handled by the layer and errors that can be handled by the hook. (no more requiring libc errno for each error!). Closes [#247](https://github.com/metalbear-co/mirrord/issues/247)

## 2.8.1

### Fixed
- CI - remove usage of ubuntu-18.04 machines (deprecated)

## 2.8.0

### Added
- E2E - add basic env tests for bash scripts

### Fixed
- mirrord-agent - Update pcap library, hopefully will fix dropped packets (syn sometimes missed in e2e).
- mirrord-agent/layer - Sometimes layer tries to connect to agent before it finsihed loading, even though pod is running. Added watching the log stream for a "ready" log message before attempting to connect.

### Changed
- E2E - describe all pods on failure and add file name to print of logs.
- E2E - print timestamp of stdout/stderr of `TestProcess`.
- E2E - Don't delete pod/service on failure, instead leave them for debugging.
- mirrord-agent - Don't use `tokio::spawn` for spawning `sniffer` (or any other namespace changing task) to avoid namespace-clashing/undefined behavior. Possibly fixing bugs.
- Change the version check on the VS Code extension to happen when mirrord is enabled rather than when the IDE starts up.


## 2.7.0

### Added
- mirrord-layer: You can now pass `MIRRORD_AGENT_COMMUNICATION_TIMEOUT` as environment variable to control agent timeout.
- Expand file system operations with `access` and `faccessat` hooks for absolute paths

### Fixed
- Ephemeral Containers didn't wait for the right condition, leading to timeouts in many cases.
- mirrord-layer: Wait for the correct condition in job creation, resolving startup/timeout issues.
- mirrord-layer: Add a sleep on closing local socket after receiving close to let local application respond before closing.
- mirrord-layer: Fix DNS issue where `ai_addr` would not live long enough (breaking the remote DNS feature).

### Changed
- Removed unused dependencies from `mirrord-layer/Cargo.toml`. (Closes #220)
- reduce e2e flakiness (add message sent on tcp listen subscription, wait for that message)
- reduce e2e flakiness - increase timeout time
- mirrord-layer - increase agent creation timeout (to reduce e2e flakiness on macOS)
- E2E - Don't do file stuff on http traffic to reduce flakiness (doesn't add any coverage value..)
- mirrord-layer - Change tcp mirror tunnel `select` to be biased so it flushes all data before closing it (better testing, reduces e2e flakiness)
- E2E - unify resolve_node_host for linux and macOS with support for wsl provided Docker & Kubernetes
- E2E - add `trace` for tests to have paramaterized arguments printed
- mirrord-agent - add debug print of args to identify runs
- E2E - remove double `--extract-path` parameter in tests
- E2E - macOS colima start with 3 cores and 8GB of RAM.
- E2E - Increase agent communication timeout to reduce flakiness.
- mirrord-layer - add `DetourGuard` to prevent unwanted calls to detours from our code.
- mirrord-layer - extract reused detours to seperate logic functions
- E2E - macOS run only sanity http mirror traffic with Python


## 2.6.0

### Added
- Add a flag for the agent, `--ephemeral-container`, to correctly refer to the filesystem i.e. refer to root path as `/proc/1/root` when the flag is on, otherwise `/`.
- Add support for Golang on amd64 (x86-64).

### Changed
- Assign a random port number instead of `61337`. (Reason: A forking process creates multiple agents sending traffic on the same port, causing addrinuse error.)
- `mirrord-layer/socket` now uses `socket2::SockAddr` to comply with Rust's new IP format.

### Fixed
- Fix filesystem tests to only run if the default path exists.
- Fix extension not running due to the node_modules directory not being packaged.

## 2.5.0

### Added
- New feature, [remote DNS resolving](https://github.com/metalbear-co/mirrord/issues/27#issuecomment-1154072686).
It is now possible to use the remote's `addrinfo` by setting the `MIRRORD_REMOTE_DNS` variable to
`true`, or using the `-d` option in mirrord-cli.
- New feature, [Ephemeral Containers](https://github.com/metalbear-co/mirrord/issues/172).
Use Kubernetes beta feature `Ephemeral Containers` to mirror traffic with the `--ephemeral-container` flag.
- E2E tests on macos for Golang using the Gin framework.

### Changed
- Refactored `mirrord-layer/socket` into a module structure similar to `mirrord-layer/file`.
- Refactored the error part of the many `Result<Response, ResponseError>`.
- Refactored `file` related functions, created `FileHandler` and improved structure.
- Refactored error handling in mirrord-layer.
- E2E: Collect minikube logs and fix collecting container logs
- E2E: macOS use colima instead of minikube.
- Refactored `mirrord-layer/lib.rs` - no more passing many arguments! :)
- Refactored `mirrord-layer/lib.rs` - remove `unwrap()` and propagate error using `Result`

### Fixed
- Handle unwraps in fileops to gracefully exit and enable python fileops tests.
- Changed `addrinfo` to `VecDeque` - fixes a potential bug (loss of order)

## 2.4.1

### Added
- mirrord-cli `exec` subcommand accepts `--extract-path` argument to set the directory to extract the library to. Used for tests mainly.
- mirrord-layer provides `MIRRORD_IMPERSONATED_CONTAINER_NAME` environment variable to specify container name to impersonate. mirrord-cli accepts argument to set variable.
- vscode-ext provides quick-select for setting `MIRRORD_IMPERSONATED_CONTAINER_NAME`

### Changed
- Refactor e2e, enable only Node HTTP mirroring test.
- E2E: add macOS to E2E, support using minikube by env var.
- E2E: Skip loading to docker before loading to minikube (load directly to minikube..)
- layer: Environment variables now load before process starts, no more race conditions.

### Fixed
- Support connections that start with tcp flags in addition to Syn (on macOS CI we saw CWR + NS)
- `fcntl` error on macOS [#184](https://github.com/metalbear-co/mirrord/issues/184) by a workaround.

## 2.3.1

### Changed
- Refactor(agent) - change `FileManager` to be per peer, thus removing the need of it being in a different task, moving the handling to the peer logic, change structure of peer handling to a struct.
- Don't fail environment variable request if none exists.
- E2E: Don't assert jobs and pods length, to allow better debugging and less flakiness.
- Refactor(agent) - Main loop doesn't pass messages around but instead spawned peers interact directly with tcp sniffer. Renamed Peer -> Client and ClientID.
- Add context to agent/job creation errors (Fixes #112)
- Add context to stream creation error (Fixes #110)
- Change E2E to use real app, closes [#149](https://github.com/metalbear-co/mirrord/issues/149)

## 2.3.0

### Added

- Add support for overriding a process' environment variables by setting `MIRRORD_OVERRIDE_ENV_VARS` to `true`. To filter out undesired variables, use the `MIRRORD_OVERRIDE_FILTER_ENV_VARS` configuration with arguments such as `FOO;BAR`.

### Changed

- Remove `unwrap` from the `Future` that was waiting for Kube pod to spin up in `pod_api.rs`. (Fixes #110)
- Speed up agent container image building by using a more specific base image.
- CI: Remove building agent before building & running tests (duplicate)
- CI: Add Docker cache to Docker build-push action to reduce build duration.
- CD release: Fix universal binary for macOS
- Refactor: Change protocol + mirrord-layer to split messages into modules, so main module only handles general messages, passing down to the appropriate module for handling.
- Add a CLI flag to specify `MIRRORD_AGENT_TTL`
- CI: Collect mirrord-agent logs in case of failure in e2e.
- Add "app" = "mirrord" label to the agent pod for log collection at ease.
- CI: Add sleep after local app finishes loading for agent to load filter make tests less flaky.
- Handle relative paths for open, openat
- Fix once cell renamings, PR [#98165](https://github.com/rust-lang/rust/pull/98165)
- Enable the blocking feature of the `reqwest` library

## 2.2.1

### Changed

- Compile universal binaries for MacOS. (Fixes #131)
- E2E small improvements, removing sleeps. (Fixes #99)

## 2.2.0

### Added

- File operations are now available behind the `MIRRORD_FILE_OPS` env variable, this means that mirrord now hooks into the following file functions: `open`, `fopen`, `fdopen`, `openat`, `read`, `fread`, `fileno`, `lseek`, and `write` to provide a mirrored file system.
- Support for running x64 (Intel) binary on arm (Silicon) macOS using mirrord. This will download and use the x64 mirrord-layer binary when needed.
- Add detours for fcntl/dup system calls, closes [#51](https://github.com/metalbear-co/mirrord/issues/51)

### Changed

- Add graceful exit for library extraction logic in case of error.
- Refactor the CI by splitting the building of mirrord-agent in a separate job and caching the agent image for E2E tests.
- Update bug report template to apply to the latest version of mirrord.
- Change release profile to strip debuginfo and enable LTO.
- VS Code extension - update dependencies.
- CLI & macOS: Extract to `/tmp/` instead of `$TMPDIR` as the executed process is getting killed for some reason.

### Fixed

- Fix bug that caused configuration changes in the VS Code extension not to work
- Fix typos

## 2.1.0

### Added

- Prompt user to update if their version is outdated in the VS Code extension or CLI.
- Add support for docker runtime, closes [#95](https://github.com/metalbear-co/mirrord/issues/95).
- Add a keep-alive to keep the agent-pod from exiting, closes [#63](https://github.com/metalbear-co/mirrord/issues/63)

## 2.0.4

Complete refactor and re-write of everything.

- The CLI/VSCode extension now use `mirrord-layer` which loads into debugged process using `LD_PRELOAD`/`DYLD_INSERT_LIBRARIES`.
  It hooks some of the syscalls in order to proxy incoming traffic into the process as if it was running in the remote pod.
- Mono repo
- Fixed unwraps inside of [agent-creation](https://github.com/metalbear-co/mirrord/blob/main/mirrord-layer/src/lib.rs#L75), closes [#191](https://github.com/metalbear-co/mirrord/issues/191)
