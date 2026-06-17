# `mirrord-layer-core` implementation plan

This document lays out a concrete, file-by-file refactor to introduce a Unix-only `mirrord-layer-core` crate that owns the shared layer bootstrap/infrastructure, while keeping `mirrord-layer-lib` as the cross-platform shared logic crate.

## Goal

Split the current Unix layer into:

- `mirrord-layer-lib`: cross-platform shared logic for Unix and Windows
- `mirrord-layer-core`: Unix layer bootstrap and hook orchestration
- `mirrord-layer`: the full Unix cdylib using core
- `mirrord-layer-remote`: a smaller Unix cdylib using the same core, later limited to bind/listen hooks

The key point is that `mirrord-layer-core` should contain only Unix-layer infrastructure, not feature-specific logic.

---

## Phase 1: create the new Unix core crate

### 1. Add a new crate directory

Create:

- `mirrord/layer/core/`
- `mirrord/layer/core/src/`

### 2. Add `mirrord/layer/core/Cargo.toml`

This crate should be a normal Rust library crate, not a `cdylib`.

Dependencies should include the Unix bootstrap/runtime pieces only:

- `mirrord-config`
- `mirrord-protocol`
- `mirrord-intproxy-protocol`
- `mirrord-layer-lib`
- `mirrord-layer-macro` if the macros remain there temporarily, or later the core crate should own them
- `ctor`
- `libc`
- `tracing`
- `nix`
- `frida-gum`

Keep the crate Unix-only with `#![cfg(unix)]` in `src/lib.rs`.

---

## Phase 2: move the Unix bootstrap modules into `mirrord-layer-core`

### 3. Move `mirrord/layer/src/hooks.rs` to `mirrord/layer/core/src/hooks.rs`

This file is the right fit for core because it only manages Frida hook installation plumbing.

Keep here:

- `HookManager`
- `hook_any_lib_export`
- `hook_export_or_any`
- Linux module/symbol resolution helpers
- the `Drop` impl that ends the transaction

Do not keep feature-specific hook code here.

### 4. Move `mirrord/layer/src/load.rs` to `mirrord/layer/core/src/load.rs`

This file is also core bootstrap logic.

Keep here:

- `ExecuteArgs`
- `LoadType`
- process filtering / load decision logic
- `should_load`
- `load_type`
- `to_process_info`

This is part of deciding how the Unix layer should start, not part of any individual feature.

### 5. Move `mirrord/layer/src/macros.rs` to `mirrord/layer/core/src/macros.rs`

This file contains the hook wiring macros used by multiple Unix modules.

Keep here:

- `replace!`
- `replace_with_fallback!`
- `hook_symbol!`
- `graceful_exit!`

If the macros remain shared between the full and remote Unix layers, they belong in core.

---

## Phase 3: add the core entrypoint

### 6. Create `mirrord/layer/core/src/lib.rs`

This file should export the core bootstrap API for Unix layer crates.

Recommended exports:

- `pub mod hooks;`
- `pub mod load;`
- `pub mod macros;`

Recommended re-exports:

- `pub use hooks::HookManager;`
- `pub use load::{ExecuteArgs, LoadType};`
- `pub use macros::{graceful_exit, hook_symbol, replace, replace_with_fallback};`

This file should own the Unix bootstrap flow currently embedded in `mirrord-layer/src/lib.rs`, including:

- `FAILSAFE_ENV`
- `EXECUTABLE_ARGS`
- `EXECUTABLE_PATH`
- `PROXY_CONNECTION_TIMEOUT`
- `layer_pre_initialization()`
- `load_only_layer_start()`
- `layer_start()` or a similarly named startup function
- the `#[ctor]` entrypoint

The core crate should provide a small API for the full and remote layer crates to call into, rather than hardcoding feature-specific behavior itself.

---

## Phase 4: shrink the current full layer crate to feature logic

### 7. Edit `mirrord/layer/src/lib.rs`

This is the main file that should lose the Unix bootstrap machinery.

Remove from this file:

- the local `mod hooks;`
- the local `mod load;`
- the local `mod macros;`
- the `HookManager` import from `crate::hooks`
- the `ExecuteArgs` import from `crate::load`
- the `#[ctor]` entrypoint
- `layer_pre_initialization()`
- `load_only_layer_start()`
- `layer_start()` if the logic is moved into core
- `FAILSAFE_ENV`
- `EXECUTABLE_ARGS`
- `EXECUTABLE_PATH`
- `PROXY_CONNECTION_TIMEOUT`

Keep in this file:

- full feature module declarations
- full layer-specific hook enabling logic
- any code that is genuinely feature-specific, such as file, DNS, outgoing, exec, TLS, and Go support

Replace the bootstrap calls with imports from `mirrord-layer-core`.

### 8. Update startup flow in `mirrord/layer/src/lib.rs`

Change the full layer so that it calls into the core crate for the generic Unix startup and then supplies its own hook-installation closure or function.

The intended direction is:

- core handles load/config/session/bootstrap
- the full layer provides `enable_hooks(...)` or equivalent feature wiring

This is the important architectural boundary.

---

## Phase 5: redirect feature modules to use core infrastructure

### 9. Edit `mirrord/layer/src/socket/hooks.rs`

Update imports so that the file uses the core crate for bootstrap infrastructure.

Specifically, replace local imports like:

- `crate::hooks::HookManager`
- `crate::replace`

with core imports such as:

- `mirrord_layer_core::hooks::HookManager`
- `mirrord_layer_core::replace`

### 10. Edit `mirrord/layer/src/socket/ops.rs`

If this file indirectly depends on the moved macros or hook manager, point those references at core.

Do not move the socket feature logic itself yet.

### 11. Edit `mirrord/layer/src/file/hooks.rs`

If it uses `replace!` or `HookManager`, import those from `mirrord-layer-core`.

### 12. Edit `mirrord/layer/src/exec_hooks.rs`

If it uses `replace!`, `HookManager`, or other moved core helpers, update imports to core.

### 13. Edit `mirrord/layer/src/tls.rs`

Only update this file if it references the moved hook infrastructure.

### 14. Edit `mirrord/layer/src/exec_utils.rs` only if needed

This file is not core by default, but if it references bootstrap helpers or macros that were moved, update those references.

---

## Phase 6: keep feature modules in the full layer for now

### 15. Do not move these files in the first extraction

Keep these in `mirrord-layer` initially:

- `mirrord/layer/src/common.rs`
- `mirrord/layer/src/file.rs`
- `mirrord/layer/src/socket.rs`
- `mirrord/layer/src/socket/hooks.rs`
- `mirrord/layer/src/socket/ops.rs`
- `mirrord/layer/src/exec_hooks.rs`
- `mirrord/layer/src/exec_utils.rs`
- `mirrord/layer/src/tls.rs`
- `mirrord/layer/src/go/*`

These are feature modules, not Unix bootstrap infrastructure.

### 16. Leave `mirrord/layer/src/common.rs` in place for now

This file is not a good first extraction target because it depends on layer-specific types and modules such as:

- `crate::exec_hooks::Argv`
- `crate::file::OpenOptionsInternalExt`
- `crate::socket::SHARED_SOCKETS_ENV_VAR`

It can be revisited later if a second extraction phase makes sense.

---

## Phase 7: wire the new crate into the workspace

### 17. Update `Cargo.toml` workspace membership

Add `mirrord/layer/core` to the workspace members so Cargo sees the new crate.

### 18. Add dependency edges

Make both layer cdylibs depend on the new core crate:

- `mirrord-layer` -> `mirrord-layer-core`
- `mirrord-layer-remote` -> `mirrord-layer-core`

Keep both crates depending on `mirrord-layer-lib` for shared cross-platform logic.

---

## Suggested final ownership after Phase 1

### `mirrord-layer-lib`
Cross-platform shared logic:

- detours
- errors
- proxy connection
- setup data structures
- socket state and protocol types
- shared socket/DNS helpers

### `mirrord-layer-core`
Unix layer bootstrap / orchestration:

- ctor entrypoint
- load detection
- startup sequencing
- hook manager
- hook registration macros
- Unix layer infrastructure

### `mirrord-layer`
Full Unix feature layer:

- file hooks
- socket hooks
- outgoing hooks
- DNS hooks
- exec hooks
- TLS hooks
- Go hooks
- macOS-specific startup behavior

### `mirrord-layer-remote`
Minimal Unix layer:

- bootstrap from core
- socket bind/listen hooks
- later, accept-family hooks if needed for working listeners

---

## Minimum viable first extraction

If you want the smallest useful version of `mirrord-layer-core`, do only these moves first:

1. `mirrord/layer/src/hooks.rs` -> `mirrord/layer/core/src/hooks.rs`
2. `mirrord/layer/src/load.rs` -> `mirrord/layer/core/src/load.rs`
3. `mirrord/layer/src/macros.rs` -> `mirrord/layer/core/src/macros.rs`
4. create `mirrord/layer/core/src/lib.rs`
5. update `mirrord/layer/src/lib.rs` to import and call core

That gives you a real Unix infrastructure crate without disrupting the feature modules yet.
