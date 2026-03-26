## Quick Reference

```bash
cargo check -p mirrord-layer --keep-going
cargo test -p mirrord-layer

# Proc-macro crate
cargo check -p mirrord-layer-macro --keep-going
```

## Architecture

### How Loading Works

The library constructor is `mirrord_layer_entry_point` (`#[ctor]` in `src/lib.rs`). On startup it reads the resolved config and process metadata (`ExecuteArgs::from_env`).

`LoadType` is computed in `src/load.rs`:
1. `Full`: normal hooking + proxy session.
2. `Skip`: no syscall interception, but still establishes a lightweight intproxy session (unless trace-only mode). This is the result when the process is a build tool (`skip_build_tools`/`skip_extra_build_tools` config) or matches `skip_processes`. On macOS, build tools get `SIPOnly` instead.
3. `SIPOnly` (macOS): only does exec/SIP patching. Applied to build tools on macOS so child processes they spawn still get the layer injected.

`MIRRORD_DONT_LOAD` hard-stops loading entirely.

### Startup Sequence

`layer_start` (`src/lib.rs`) does:
1. Init tracing.
2. Init global setup (`init_layer_setup(config, false)`).
3. Register hooks (`enable_hooks`).
4. Create intproxy session (`ProxyConnection::new` + `NewSessionRequest`).
5. Optionally fetch and apply remote env vars.

Hook registration is driven by config:
- File hooks only when the fs feature is active.
- DNS hooks only when remote DNS is enabled.
- Exec hooks on macOS, or on Linux with experimental toggle.
- Go hooks on Linux `x86_64`/`aarch64`.

### Hooking Infrastructure

`HookManager` wraps Frida interceptor transactions (`begin_transaction` on creation, `end_transaction` on drop). `replace!` (and `replace_with_fallback!`) resolves a symbol, installs the detour, and stores the original function pointer.

`#[hook_fn]` and `#[hook_guard_fn]` from `mirrord-layer-macro` generate:
1. `Fn*` type alias
2. `FN_*` global storage for the original function
3. Optional `DetourGuard` pre-check (`hook_guard_fn`)

The detour return model comes from `mirrord_layer_lib::detour`: `Success`, `Bypass`, `Error`.

### Shared State

- `OPEN_FILES`: maps local fd → `Arc<RemoteFile>`
- `SOCKETS`: maps local fd → `Arc<UserSocket>`
- `OPEN_DIRS`: tracks open remote directory streams and stable dirent buffers
- `MANAGED_ADDRINFO`: tracks layer-allocated `addrinfo` chains to decide whether `freeaddrinfo` is custom or libc

Dup semantics rely on `Arc` ownership, so remote resources are closed only when the final reference is dropped.

## Rules to Follow

- Keep hook registration complete for each feature gate; missing variants often break specific runtimes (Node/libuv/Go).
- Any new detour that allocates C-owned memory must have a clear free path.
- Be careful with lock ordering and fork paths. `fork_detour` grabs major layer locks before fork to avoid inherited-locked-mutex deadlocks. Deadlocks are easy to introduce.
- Don't log from `RemoteFile::Drop` (lock re-entry deadlock risk) or `close_layer_fd` (can run while stdio fds are closing).
- Go hook targets differ by Go version (pre-1.19, 1.19+, 1.23+, 1.25+, 1.26 symbol moves). When changing Go hooks, re-check runtime symbol names and preserve register/stack ABI assumptions in naked assembly.
