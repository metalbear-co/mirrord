# mirrord

## Overview

mirrord is a tool that lets developers run local processes in the context of their Kubernetes cloud environment.

`CONTRIBUTING.md` has the procedures: preparing a cluster for the end-to-end tests, debugging with `mirrord-console`,
building the agent on macOS, adding a hook or a target type, and releasing.

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
- `mirrord-mcp`: the MCP server behind `mirrord mcp`, which exposes mirrord to AI agents over stdio.

## Command Reference

### Compiling

```bash
# check the entire workspace at once
# always use when a change spans multiple crates instead of checking each crate individually
# CI appends `-- -Wclippy::indexing_slicing` to every clippy run, so add it to any clippy command here
CARGO_BUILD_WARNINGS=deny cargo clippy --all-targets --all-features --keep-going -- -Wclippy::indexing_slicing

# check a specific crate
# the agent is linux only
CARGO_BUILD_WARNINGS=deny cargo clippy -p mirrord-agent --target x86_64-unknown-linux-gnu --keep-going -- -Wclippy::indexing_slicing

# rustdoc of private items, as `check-rust-docs` builds it
RUSTDOCFLAGS=-Dwarnings cargo xtask test-doc

# layer and cli bundled with the new layer
# use only when a fresh mirrord + layer binary is needed for testing
cargo xtask build-cli
```

Use `--keep-going` to surface all errors at once. Use `clippy` instead of `check` to lint + compile simultaneously.

### Testing

```bash
# integration tests
# builds the cli and layer, unless given `--binary`/`--layer` or `MIRRORD_TESTS_USE_BINARY`/`MIRRORD_LAYER_FILE`
cargo xtask test-integration

# filtered integration tests
cargo xtask test-integration -- outgoing_udp --nocapture

# e2e tests, against the cluster kubectl points at; same artifact options
cargo xtask test-e2e

# unit tests of the cli, with placeholder layer files
# other crates: cargo nextest run -p <package>
cargo xtask test-ut

# filtered unit tests
cargo xtask test-ut -- resolve_url_happy_path -- --nocapture

# any of these inside the CI runner image, which has every toolchain the suites need
cargo xtask in-runner -- cargo xtask test-e2e
```

Never write unit tests unless explicitly asked. Run the existing tests to verify a change.

Prefer an integration test (`mirrord-layer-tests`, which stands in for the agent) to an end-to-end test, which needs a
cluster. Don't add the `#[timeout]` of `rstest`: `.config/nextest.toml` already times out every test and reports it as
a timeout, where `rstest` panics like any other failure.

#### A test can merge and never run

CI runs only what it names: every job is package-scoped and there is no workspace-wide run. Before relying on
a new test, pull the job log and grep for its name rather than trusting a green check.

Grepping the workflows for `-p <package>` is not enough either, since some runs hide behind xtask.
`mirrord/cli/`, whose package is plain `mirrord`, is tested on Linux by `cargo xtask test-ut` in
`.github/workflows/test-integration.yaml` and on Windows by `cargo nextest run -p mirrord ...` in
`test-windows.yaml`. macOS never runs those unit tests: it builds the binary and exercises it only through
`mirrord-layer-tests`, so a macOS-gated test in `mirrord/cli/` never runs while the PR stays green.

### Styling

```bash
# Formatting
cargo fmt
```

## CI

`.github/workflows/ci.yaml` runs on pull requests into `main` that aren't drafts, and in the merge queue. A pull
request into a branch its `pull_request` trigger doesn't list gets it only as part of a GitHub stack, from the first
push after it joins. Its `plan` job picks the other jobs from the changed paths, and a change under
`.github/workflows/` runs nearly every one.

- `ci` (the `ci-success` job) is the only required check. It and `lint-success` only collect the jobs they need, so
  when either fails, read the job under it that failed.
- `main` merges through the merge queue, with merge commits, after one approval and with every review thread resolved.
- nextest retries a failing test twice, so `FLAKY` marks a pass on a retry and `TRY 3 FAIL` a test that never passed.
- Clippy runs for macOS, Windows, and Linux targets, so code behind a `cfg` can fail on one of them only.
- `typos` checks the tracked Markdown outside `mirrord/mcp/corpus`, and Vale the Markdown at the root and every fragment
  in `changelog.d`. `CONTRIBUTING.md` says how to clear a false positive from either.
- The CI runner image (`tests/e2e.Dockerfile`) bakes in the compiled test apps (`scripts/prepare_e2e.sh --apps-only`).
  A pull request rebuilds it when a path in `plan`'s `changed-runner-image` filter changes, and `main` republishes it
  when one in `publish-ci-runner.yaml`'s paths does. A new kind of compiled app goes in the script and both lists.
- While the `windows-e2e` label is on a pull request, adding any label runs the Windows end-to-end tests; pushes
  don't. `no-greptile` skips Greptile's review, and `no-qodo` skips Qodo's only if it's on before the pull request
  leaves draft (`.pr_agent.toml`).

## Changelog Fragments

Every pull request adds a fragment to `changelog.d/`, or `towncrier check` fails it. Name it `<issue>.<type>.md` after
an issue in this repository, or `+<name>.<type>.md` when there's none; never use the ID of a private tracker. The
types are the `[[tool.towncrier.type]]` directories in `towncrier.toml`: `internal` is for what users never see,
such as tests and CI, and any type besides `fixed`, `security`, and `internal` makes the next release a minor one.

The fragment becomes the release notes, so write it for users: what changed for them, not in the code. Never edit
`CHANGELOG.md`; the release pull request builds it.

## Compatibility

The CLI ships the layer and the intproxy inside its own binary, but not the agent or the operator. An agent can be
another version (a custom `agent.image`, or one the operator spawns), and users upgrade the operator on their own
schedule. What's only on `main` hasn't shipped and needs no compatibility; what's in the latest release tag does.

- New messages to the agent are gated on the negotiated protocol version (`mirrord/protocol/AGENTS.md`).
- A capability the CLI newly asks of the operator is gated on `require_feature(NewOperatorFeature::...)`, so an older
  operator fails with a clear error. Without the gate it may ignore the request, reject it, or leave the CLI waiting
  on a resource nothing reconciles.
- An exception: a feature the operator started advertising after the capability shipped can't be gated, since an
  operator that supports the capability may not advertise it. The CLI relies on the operator's own failure instead
  (`required_branching_feature`).
- An older CLI must keep working against a newer operator, so new behavior is opt-in and no client has to change. The
  rules this puts on the shared CRD types are in `mirrord/operator/AGENTS.md`.

## Related Repositories

- `metalbear-co/docs` documents user-visible behavior, so a change users notice usually needs a pull request there
  too. The configuration reference is the exception: it's generated here (`mirrord/config/AGENTS.md`).
- `metalbear-co/mirrord-vscode` and `metalbear-co/mirrord-intellij` drive the CLI; `ci.yaml` runs their end-to-end
  tests on release branches.
- `metalbear-co/test-images` builds the app images the end-to-end tests deploy.

## Generated Files

`mirrord-schema.json` and `mirrord-up-schema.json` are generated, as `mirrord/config/AGENTS.md` and
`mirrord/up/AGENTS.md` say. A change to either runs `corpus_check`, which validates the config examples in the skills
`mirrord-mcp` vendors against them, so renaming or removing an option they use fails it.

## Simplicity and Reuse

mirrord is actively maintained by dozens of people, it is not a greenfield project. When adding or changing things,
maximize simplicity. Reuse existing abstractions and codepaths instead of introducing new ones. Every new path is
something someone has to understand, maintain, and keep compatible.

Avoid extracting trivial, self-contained logic into a function used only once. Keep it near its point of use when the
helper would merely add indirection or exist to make a tiny test possible. Extract a function when it is reused or hides
non-obvious details that would distract from the surrounding flow. Test meaningful behavior rather than trivial
wrappers around a library API.

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

## Stack Usage

The CLI polls its whole runtime on the main thread, and on Windows that thread only gets the `/STACK` linked in
`mirrord/cli/build.rs`. Debug builds are hit hardest: a function's frame keeps a slot for every future it builds
before awaiting it, so one `match` with a large future per arm pays for all of them.

- Put `#[mirrord_macros::boxed_future]` (above `#[tracing::instrument]`) on an `async fn` with a large future, such as
  a subcommand or a connection step: it builds its future on the heap, and callers keep writing `f(..).await`.
  `Box::pin(f(..))` and `.boxed()` don't help in a debug build, since `f(..)` is still built in the caller's frame first.
- On a stack overflow, the Windows CLI prints every frame with its size, largest first. Windows CI runs
  `scripts/win_stack_check.ps1`, which fails when the CLI outgrows its budget and shows that report.
- `scripts/win_stack_report.ps1` measures peak stack and large frames locally.

## Style Guideline

- Keep imports at file top (no function-local `use`).
- Prefer `to_owned` for `&str` → `String`.
- Always use `foo.rs` instead of `foo/mod.rs` for module roots.
- Run styling commands after every edit.
- `STYLE.md` has more Rust conventions; follow it too.
