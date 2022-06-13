# Change Log

All notable changes to the mirrord's cli, agent, protocol, extensions will be documented in this file.
Previous versions had CHANGELOG per component, we decided to combine all repositories to a mono-repo with one CHANGELOG.

Check [Keep a Changelog](http://keepachangelog.com/) for recommendations on how to structure this file.

## [Unreleased]

### Changed
- Speed up agent container image building by using a more specific base image.
- CI: Remove building agent before building & running tests (duplicate)

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
