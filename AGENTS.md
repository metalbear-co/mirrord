# mirrord

## Project Overview

mirrord is a tool that lets developers run local processes in the context of their Kubernetes cloud environment.

### Core Crates

- `mirrord-layer`: gets loaded into the user's process via `LD_PRELOAD`/`DYLD_INSERT_LIBRARIES`. Intercepts libc
calls (files, networking, DNS, env, ...) and turns them into protocol messages sent to intproxy.
- `mirrord-intproxy`: sits between the layers and the agent. Matches requests to responses, handles reconnects,
and routes messages to the right feature proxy.
- `mirrord-agent`: runs in the Kubernetes cluster in the target pod's context and network namespace. Does the actual
I/O work requested by the layers.
- `mirrord-protocol`: defines the shared messages (`ClientMessage`, `DaemonMessage`) sent between
layer/intproxy/agent.
- `mirrord-config`: config types and validation. Used by the CLI, layer, and intproxy to decide what features are
enabled and what target to use.

## Command Reference

### Compiling

```bash
# check the entire workspace at once
# use when a change spans multiple crates
cargo check --all-targets --keep-going

# layer and cli bundled with the new layer
cargo xtask build-cli

# standalone cli bundled with a pre-built layer
cargo check -p mirrord --keep-going

# layer + shims + universal binary on macos
cargo xtask build-layer

# standalone layer
cargo check -p mirrord-layer --keep-going

# intproxy
cargo check -p mirrord-intproxy --keep-going

# agent (Linux only)
cargo check -p mirrord-agent --target x86_64-unknown-linux-gnu --keep-going
```

Use `cargo check -p <crate> --keep-going` to surface all errors at once rather than stopping at the first.

### Testing

```bash
# integration tests
# requires building the cli first with `xtask build-cli`
cargo xtask test-integration

# filtered integration tests
cargo xtask test-integration -- outgoing_udp --nocapture

# unit tests (cargo test)
cargo xtask test-ut

# filtered unit tests
cargo xtask test-ut -- resolve_url_happy_path -- --nocapture
```

### Styling

```bash
# Formatting
cargo fmt

# Linting
cargo clippy --all-targets -- --deny warnings
```

## Simplicity and Reuse

mirrord is actively maintained by dozens of people, it is not a greenfield project. When adding or changing things,
maximize simplicity. Reuse existing abstractions and codepaths instead of introducing new ones. Every new path is
something someone has to understand, maintain, and keep compatible.

## Comments and Documentation

Don't write comments explaining what code does, the code should speak for itself. Instead, focus on _why_ something
exists: doc comments that explain the problem a function or struct solves and how it fits into the bigger picture are
far more valuable than ones that restate the type signature. Module-level doc comments that give a high-level overview
of what a file is responsible for are also very helpful, as they give readers the context to understand the code below.
mirrord has a lot of moving pieces and stability is critical, so document tricky or non-obvious behavior clearly.

When changing how something works, don't leave comments like "this replaces how X used to do it" just document the new
behavior directly.

## Style Guideline

- Keep imports at file top (no function-local `use`).
- Prefer `to_owned` for `&str` → `String`.
- Always use `foo.rs` instead of `foo/mod.rs` for module roots.
- Run styling and linting commands after every edit.
