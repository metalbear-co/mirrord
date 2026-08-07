# mirrord

## Overview

mirrord is a tool that lets developers run local processes in the context of their Kubernetes cloud environment.

### Core Crates

- `mirrord-layer`: gets loaded into the user's process via `LD_PRELOAD`/`DYLD_INSERT_LIBRARIES`. Intercepts libc
calls (files, networking, DNS, env, ...) and turns them into protocol messages sent to the intproxy.
- `mirrord-intproxy`: a local process spawned by the CLI that bridges N layers and 1 agent. Matches requests to
responses, handles reconnects, and routes messages to the right feature proxy.
- `mirrord-agent`: the in-cluster component of mirrord. Runs in the target pods' context and network namespace. Does the
actual I/O work requested by the layers.
- `mirrord-protocol`: defines the shared messages (`ClientMessage`, `DaemonMessage`) sent between layer/intproxy/agent.
- `mirrord-config`: config types and validation. Used by the CLI and layer to decide what features are enabled and what
target to use.
- `mirrord`: the CLI that resolves configuration, creates or connects to the agent, starts the intproxy, and launches
the user's local process with the mirrord layer loaded.

## Command Reference

### Compiling

```bash
# check the entire workspace at once
# always use when a change spans multiple crates instead of checking each crate individually
cargo clippy --all-targets --all-features --keep-going -- --deny warnings

# check a specific crate
# the agent is linux only
cargo clippy -p mirrord-agent --target x86_64-unknown-linux-gnu --keep-going -- --deny warnings

# layer and cli bundled with the new layer
# use only when a fresh mirrord + layer binary is needed for testing
cargo xtask build-cli
```

Use `--keep-going` to surface all errors at once. Use `clippy` instead of `check` to lint + compile simultaneously.

### Testing

```bash
# integration tests
# requires building the cli first with `xtask build-cli`
cargo xtask test-integration

# filtered integration tests
cargo xtask test-integration -- outgoing_udp --nocapture

# unit tests
cargo xtask test-ut

# filtered unit tests
cargo xtask test-ut -- resolve_url_happy_path -- --nocapture
```

### Styling

```bash
# Formatting
cargo fmt
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

Write comments for readers of the resulting code, not reviewers of the patch. Comments must stand alone after merge and
describe the current behavior, intent, invariant, or non-obvious tradeoff. Don't narrate the edit or use patch-relative
language such as `the existing implementation`, `the new approach`, `now`, `previously`, `this change`, or
`we added/removed`. Include historical context only when it is necessary to explain why the current code deliberately
differs from an otherwise-obvious alternative.

## Style Guideline

- Keep imports at file top (no function-local `use`).
- Prefer `to_owned` for `&str` → `String`.
- Always use `foo.rs` instead of `foo/mod.rs` for module roots.
- Run styling commands after every edit.
