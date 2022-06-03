# Change Log

All notable changes to the mirrord's cli, agent, protocol, extensions will be documented in this file.
Previous versions had CHANGELOG per component, we decided to combine all repositories to a mono-repo with one CHANGELOG.

Check [Keep a Changelog](http://keepachangelog.com/) for recommendations on how to structure this file.

## [Unreleased]

### Added

- Prompt user to update if their version is outdated in the VS Code extension or CLI.
- Set `MIRRORD_CHECK_VERSION` to false to make E2E tests not read update messages.
- File operations behind `MIRRORD_FILE_OPS` env variable.

## 2.0.0

Complete refactor and re-write of everything.

- The CLI/VSCode extension now use `mirrord-layer` which loads into debugged process using `LD_PRELOAD`/`DYLD_INSERT_LIBRARIES`.
  It hooks some of the syscalls in order to proxy incoming traffic into the process as if it was running in the remote pod.
- Mono repo
