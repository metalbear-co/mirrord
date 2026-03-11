# Layer Tests Migration to mirrord-layer-tests

## Overview
Goal: move all integration tests from `mirrord/layer/tests/*.rs` to `mirrord/layer-tests/tests/`,
using the exec-based harness. Apps/configs remain under `mirrord/layer/tests/` for now.

## Phase 0 — Harness Parity
- [x] Port `Application` enum and helpers to `mirrord/layer-tests/tests/common/mod.rs`
- [x] Point app/config paths to `../layer/tests/...`
- [x] Add optional Java path override env (e.g., `MIRRORD_TEST_JAVA_PATH`) if needed

## Phase 1 — Rust/C-only Tests
- [x] connectx
- [x] dlopen_cgo
- [x] dns_resolve
- [x] double_listen
- [x] dup_listen
- [x] fork
- [x] listen_ports
- [x] mkdir_rmdir
- [x] readlink
- [x] statfs_fstatfs
- [x] issue1054
- [x] issue1123
- [x] issue1458
- [x] issue1776
- [x] issue1899
- [x] issue2001
- [x] issue2055
- [x] issue2178
- [x] issue2204
- [x] issue2438
- [x] issue2744
- [x] issue3248
- [x] outgoing (remaining cases)
- [x] recv_from
- [x] rebind0

## Phase 2 — Runtime-heavy Tests (Node/Python/Go)
- [x] http_mirroring
- [x] fileops
- [x] ignore_ports
- [x] issue2614
- [x] issue2817
- [x] issue2807
- [x] issue2283
- [x] issue3456
- [x] issue834
- [x] issue864
- [x] node_copyfile
- [x] port_mapping
- [x] self_connect
- [x] skipping_process
- [x] spawn

## Phase 3 — SIP/exec-sensitive Tests
- [x] bash
- [x] sip
- [x] java_sip

## Notes
- Remove or disable original `mirrord/layer/tests/*.rs` as each test is migrated to avoid duplicate runs.
- Keep `cargo test -p mirrord-layer` for unit tests; integration tests should live only in `mirrord-layer-tests`.
