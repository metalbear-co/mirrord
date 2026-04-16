## Quick Reference

```bash
# layer
cargo check -p mirrord-layer --keep-going
# intproxy
cargo check -p mirrord-intproxy --keep-going
# agent (Linux-only)
cargo check -p mirrord-agent --target x86_64-unknown-linux-gnu --keep-going
# CLI
cargo check -p mirrord --keep-going

# Integration tests
cargo test -p mirrord-layer-tests

# Always format after edits
cargo fmt

# Always lint after edits
cargo clippy -- --deny warnings
```

Use `cargo check -p <crate> --keep-going` to surface all errors at once rather than stopping at the first.

## Contribution Notes

- Add a towncrier fragment for code changes in `changelog.d/`, named `<identifier>.<category>.md`.
- Use a public GitHub issue number when one exists, otherwise use `+some-name`.
- Valid categories are `added`, `fixed`, `internal`, and `changed`.
- Use `internal` for test-only, CI-only, and other changes users do not care about.

## Project Overview

mirrord is a tool that lets developers run local processes in the context of their cloud environment.

### How the Pieces Connect

```
┌─────────────────────────────────────────────────────────────────────────┐
│  LOCAL MACHINE                                                          │
│  ┌──────────────────────┐     ┌──────────────────────┐                  │
│  │   User Application   │     │      CLI (mirrord)   │                  │
│  │  ┌────────────────┐  │     │  - Resolves target   │                  │
│  │  │     layer      │  │     │  - Starts intproxy   │                  │
│  │  │ (LD/DYLD hook) │◄─┼─────┤  - Provides config   │                  │
│  │  └───────┬────────┘  │     └──────────────────────┘                  │
│  └──────────┼───────────┘                                               │
│             │ local protocol (TCP/Unix)                                 │
│  ┌──────────▼───────────┐                                               │
│  │      intproxy        │  Routes messages between layer(s) and agent   │
│  └──────────┬───────────┘                                               │
└─────────────┼───────────────────────────────────────────────────────────┘
              │ agent protocol over port-forward/operator tunnel
┌─────────────┼───────────────────────────────────────────────────────────┐
│  KUBERNETES │ CLUSTER                                                   │
│  ┌──────────▼───────────┐                                               │
│  │        agent         │  Runs remote fs/network/dns/env ops           │
│  │   (target context)   │  in the target pod's context                  │
│  └──────────────────────┘                                               │
└─────────────────────────────────────────────────────────────────────────┘
```

### Core Crates

- **`mirrord-layer`**: gets loaded into the user's process via `LD_PRELOAD`/`DYLD_INSERT_LIBRARIES`. Intercepts syscalls (file, socket, DNS, exec) and sends them to intproxy. Manages local fd and socket state.
- **`mirrord-intproxy`**: sits between layers and the agent. Matches requests to responses, handles reconnects, and routes messages to the right feature proxy (files, incoming, outgoing, DNS, env).
- **`mirrord-agent`**: runs in the Kubernetes cluster in the target pod's context. Does the actual remote work: file operations, network mirroring/stealing, outgoing connections, DNS resolution, env var extraction.
- **`mirrord-protocol`**: defines the shared messages (`ClientMessage`, `DaemonMessage`) sent between layer/intproxy/agent. Uses bincode serialization. Backward compatibility is mandatory since these components ship independently.
- **`mirrord-config`**: config types and validation. Used by the CLI, layer, and intproxy to decide what features are enabled and what target to use.

### Flow of a `mirrord exec` Session

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

The layer, intproxy, and agent communicate through two message enums defined in `mirrord-protocol`: `ClientMessage` (sent by the layer/intproxy to the agent) and `DaemonMessage` (sent by the agent back). These are serialized with bincode over a TCP connection.

Between the layer and intproxy there's a separate local protocol (`mirrord-intproxy-protocol`) using `LayerToProxyMessage` and `ProxyToLayerMessage`.

## Simplicity and Reuse

mirrord is actively maintained by dozens of people, it is not a greenfield project. When adding or changing things, maximize simplicity. Reuse existing abstractions and codepaths instead of introducing new ones. Every new path is something someone has to understand, maintain, and keep compatible.

## Comments and Documentation

Don't write comments explaining what code does, the code should speak for itself. Instead, focus on _why_ something exists: doc comments that explain the problem a function or struct solves and how it fits into the bigger picture are far more valuable than ones that restate the type signature. Module-level doc comments that give a high-level overview of what a file is responsible for are also very helpful, as they give readers the context to understand the code below. mirrord has a lot of moving pieces and stability is critical, so document tricky or non-obvious behavior clearly.

When changing how something works, don't leave comments like "this replaces how X used to do it" just document the new behavior directly.

## Style Guideline

- Keep imports at file top (no function-local `use`).
- Prefer `to_owned` for `&str` → `String`.
- Always use `foo.rs` instead of `foo/mod.rs` for module roots.
- When extending existing declarations (consts, methods, enum variants, match arms), append new entries below the existing ones. If the new entry is related to a specific existing one, place it directly below that one instead of at the end.
- Run `cargo fmt` and `cargo clippy -- --deny warnings` after every edit.
