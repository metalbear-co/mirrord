# Change Log

All notable changes to the mirrord's cli, agent, protocol, extensions will be documented in this file.
Previous versions had CHANGELOG per component, we decided to combine all repositories to a mono-repo with one CHANGELOG.

Check [Keep a Changelog](http://keepachangelog.com/) for recommendations on how to structure this file.

## [Unreleased]

### Changed
- Added graceful exit for library extraction logic in case of error
- Refactor the CI by splitting the building of mirrord-agent in a separate job and caching the agent image for E2E tests.
- File operations are now available behind the `MIRRORD_FILE_OPS` env variable, this means that mirrod now hooks into the following file functions: `open`, `fopen`, `fdopen`, `openat`, `read`, `fread`, `fileno`, `lseek`, and `write` to provide a mirrored file system.

### Fixed

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
