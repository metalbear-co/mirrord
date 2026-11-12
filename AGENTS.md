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

Bazel builds and tests the workspace, with toolchains that it downloads, and CI runs the same commands (see the top of
`.github/workflows/ci.yaml`). The devshell in `flake.nix` provides `bazel` and the tools that the commands below take
from `PATH`.

### Compiling

```bash
# build everything for this machine, including the CLI with its layer and UI
# `--keep_going` surfaces all errors at once
bazel build --keep_going //...

# lint every Rust crate with clippy, as CI does on Linux, macOS and Windows, and the end-to-end
# tests, which are `manual`, so `//...` leaves them out
bazel build --config=clippy --keep_going //... //mirrord/tests:e2e

# lint the crates for another platform, without a machine that runs it, e.g. Windows from Linux or a Mac
bazel build --config=clippy --platforms=//bazel/platforms:x86_64-windows //...

# rustdoc of private items, with warnings as errors, as CI builds it
bazel build --config=doc //...

# the CLI as one file, with the layer and the UI embedded
bazel build //mirrord/cli:cli_standalone

# run the CLI that the tests run, which has no UI (`//mirrord/cli:cli_standalone` has it)
bazel run //mirrord/cli -- <args>
```

### Testing

```bash
# every unit and integration test that builds for this machine
bazel test //...

# the tests of one package, e.g. the CLI's unit tests or the layer tests (the integration tests)
bazel test //mirrord/cli:unit_tests
bazel test //mirrord/layer-tests/...

# filtered tests: `--test_filter` keeps the tests whose names contain it, and `--test_arg` passes nextest's options
bazel test //mirrord/layer-tests/... --test_filter=outgoing_udp --test_output=streamed
bazel test //mirrord/cli:unit_tests --test_arg=-E --test_arg='test(/resolve_url/)'

# e2e tests, against a kind or minikube cluster that `e2e_setup` creates or reuses (see `mirrord/tests/BUILD.bazel`),
# and `--delete` deletes the cluster
bazel run //mirrord/tests:e2e_setup
bazel test //mirrord/tests:e2e --test_env=KUBECONFIG=<the path that e2e_setup prints>
bazel run //mirrord/tests:e2e_setup -- --delete

# lint the docs and the frontends, check the changelog fragment and the dependencies, and check the formatting
bazel run //:lint
bazel run //:format -- --check
```

A unit test in a PR is never a review finding for being there. As a coding agent writing a change, add unit tests only
when explicitly asked to by the developer you work for, the change's requirements, or a review comment you're told to
address, never just because you can. Run the existing tests to verify a change.

Prefer an integration test (`mirrord-layer-tests`, which stands in for the agent) to an end-to-end test, which needs a
cluster. Don't add the `#[timeout]` of `rstest`: `.config/nextest.toml` already times out every test and reports it as
a timeout, where `rstest` panics like any other failure.

#### A test can merge and never run

Bazel only runs the tests that a `BUILD.bazel` declares: a new file under a package's `tests/` needs its own
`rust_test`, as Cargo's integration tests don't. A crate without tests has no `unit_tests` target, so its first
`#[test]` needs one too. A test target with `target_compatible_with` only builds and runs on those platforms, and a
target tagged `manual`, like the e2e targets, only runs when a command names it. Before relying on a new test, pull the
job log and grep for its name rather than trusting a green check.

### Styling

```bash
# format the Rust code, the Starlark files and the frontends
bazel run //:format
```

## CI

`.github/workflows/ci.yaml` runs on pull requests into `main` that aren't drafts, and in the merge queue. A pull
request into a branch its `pull_request` trigger doesn't list gets it only as part of a GitHub stack, from the first
push after it joins. Its `plan` job skips the build for a change to the root's Markdown and the changelog fragments
alone, and a change under `.github/` runs nearly every job.

- CI runs the same Bazel commands as a developer, which the comment at the top of `ci.yaml` lists. The
  `build-test (<platform>)` jobs build, lint, document and test on each platform, in one step each, so read the step
  that failed. `test-e2e.yaml` runs the end-to-end tests on a kind, a minikube and an IPv6 kind cluster.
- `ci` (the `ci-success` job) is the only required check. It only collects the jobs it needs, so when it fails, read
  the job under it that failed.
- `main` merges through the merge queue, with merge commits, after one approval and with every review thread resolved.
- nextest retries a failing test twice, so `FLAKY` marks a pass on a retry and `TRY 3 FAIL` a test that never passed.
- Clippy runs for macOS, Windows, and Linux targets, so code behind a `cfg` can fail on one of them only.
- `bazel run //:lint` runs `typos` on the tracked Markdown outside `mirrord/mcp/corpus`, and Vale on the Markdown at the
  root and every fragment in `changelog.d`. `CONTRIBUTING.md` says how to clear a false positive from either.
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
`mirrord/up/AGENTS.md` say. A change to either runs the `corpus` step of CI's `lint` job, which validates the config
examples in the skills `mirrord-mcp` vendors against them, so renaming or removing an option they use fails it.

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
