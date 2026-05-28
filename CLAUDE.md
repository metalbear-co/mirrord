## Project Overview

`mirrord` is a tool that lets developers run local processes in the context of their Kubernetes cloud environment.

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
# CLI
cargo xtask build-cli

# layer
cargo xtask build-layer

# intproxy
cargo check -p mirrord-intproxy --keep-going

# agent (Linux only)
cargo check -p mirrord-agent --target x86_64-unknown-linux-gnu --keep-going
```

Use `cargo check -p <crate> --keep-going` to surface all errors at once rather than stopping at the first.

### Testing

```bash
# Integration tests
cargo xtask test-integration

# Unit tests
cargo xtask test-ut
```

### Styling

```bash
# Formatting
cargo fmt

# Linting
cargo clippy -- --deny warnings
```

## Flow of a `mirrord exec` Session

1. CLI resolves the target and figures out the connection mode (operator or direct Kubernetes).
2. CLI starts intproxy and injects the layer (`LD_PRELOAD`/`DYLD_INSERT_LIBRARIES`).
3. layer initializes hooks and opens a session with intproxy.
4. intproxy connects to the agent:
   - Direct Kubernetes (in OSS/`operator = false` mode): intproxy reaches the agent via port-forward or direct tunnel.
   - Operator: intproxy joins an operator-managed session and routes through operator-provided connectivity.
5. Hooked operations become protocol requests (`ClientMessage`) sent through intproxy.
6. agent executes in the target's context and sends responses/events back (`DaemonMessage`).
7. layer translates those results back into what the syscall caller expects.

## The Protocol

The layer, intproxy, and agent communicate through two message enums defined in `mirrord-protocol`: `ClientMessage`
(sent by the intproxy to the agent) and `DaemonMessage` (sent by the agent back). These are serialized with
bincode over a TCP connection.

Between the layer and intproxy there's a separate local protocol (`mirrord-intproxy-protocol`) using
`LayerToProxyMessage` and `ProxyToLayerMessage`.

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
- Run `cargo fmt` and `cargo clippy -- --deny warnings` after every edit.

## Contribution Notes

- Add a towncrier fragment for code changes in `changelog.d/`, named `<identifier>.<category>.md`.
- Use a public GitHub issue number when one exists, otherwise use `+some-name`.
- Valid categories are `added`, `fixed`, `internal`, and `changed`.
- Use `internal` for test-only, CI-only, and other changes users do not care about.
