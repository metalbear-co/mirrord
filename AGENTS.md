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

The project is managed and built by Bazel, so compile and test with Bazel instead of Cargo.

### Compiling

```bash
# build everything
bazel build --keep_going //...

# clippy
bazel build --config=clippy --keep_going //...

# rustdoc
bazel build --config=doc //...

# run the CLI
# this will rebuild the layer, ui, and every other dependency of the CLI if necessary
bazel run //mirrord/cli -- <args>
```

### Testing

```bash
# run every unit and integration test
bazel test //...

# one package's tests, e.g. the CLI's unit tests or the layer (integration) tests
bazel test //mirrord/cli:unit_tests
bazel test //mirrord/layer-tests/...

# `--test_filter` filters tests by name and `--test_arg` passes an option to nextest
bazel test //mirrord/layer-tests/... --test_filter=outgoing_udp --test_output=streamed
bazel test //mirrord/cli:unit_tests --test_arg=-E --test_arg='test(/resolve_url/)'
```

#### E2E tests

Note that e2e tests take much longer to setup and run, so prefer running integration/unit tests instead.

```bash
# runs the e2e tests on a kind cluster, `mirrord-e2e`, which the target creates if it doesn't exist and loads the
# agent built from the checkout into; `--//tests:cluster_runtime=minikube` uses minikube instead
bazel test //tests:e2e
```

`CONTRIBUTING.md` explains the other e2e targets and how to use their clusters.

#### Guidelines

A unit test in a PR is never a review finding for being there. As a coding agent writing a change, add unit tests only
when explicitly asked to by the developer you work for, the change's requirements, or a review comment you're told to
address, never just because you can. Run the existing tests to verify a change.

Prefer an integration test (`mirrord-layer-tests`, which stands in for the agent) to an end-to-end test, which needs a
cluster. Don't add the `#[timeout]` of `rstest`: `.config/nextest.toml` already times out every test and reports it as
a timeout, where `rstest` panics like any other failure.

Bazel builds each dependency once for the whole workspace, with every feature that any `Cargo.toml` turns on,
`[dev-dependencies]` included, so a feature that only a test needs ships in the agent, the CLI and the layer too. Don't
turn on a test-only feature, like tokio's `test-util` for `#[tokio::test(start_paused = true)]`, or add a dev-dependency
that turns one on, like `tower-test`. Use short real durations, or a duration that the test passes to the code
(`CONTRIBUTING.md` has more).

#### A test can merge and never run

Bazel only runs the tests that a `BUILD.bazel` declares. `crate.integration_tests()` declares one for each file in the
package's `tests/`, but a package's first integration test needs that call, and its first `#[test]` needs a
`crate.unit_tests()`, which also runs the tests of each binary that the package's `crate.binary` calls declare. A test
target with `target_compatible_with` only builds and runs on those platforms, and a target tagged `manual`, like the
e2e targets, only runs when a command names it. Before relying on a new test, pull the job log and grep for its name
rather than trusting a green check.

### Formatting

```bash
# lint and format the entire project (rust workspace, frontends, python scripts, starlark, etc)
# pass `-- --help` for more information
bazel run //:lint
bazel run //:format
```

Both run the tools that Bazel downloads, like Ruff, typos and pnpm, at the versions in `bazel/multitool.lock.json` and
`package.json`, rather than the machine's. The end-to-end tests run Bazel's kind, minikube, kubectl and Helm too, so
a machine needs only bazelisk, Git and Docker.

## CI

`.github/workflows/ci.yaml` runs on pull requests into `main` that aren't drafts, and in the merge queue. A pull
request into a branch its `pull_request` trigger doesn't list gets it only as part of a GitHub stack, from the first
push after it joins. Its `plan` job skips the build when a pull request only changes the Markdown at the root or the
changelog fragments. A pull request that only changes `docs/`, `guides/` or `skills/` skips the build and the changelog
check, and the `lint` job runs the tests of the docs and skills instead. A change under `.github/` runs nearly every
job.

- The `build-test (<platform>)` jobs build, lint, document and test, each in a step of its own, so read the step that
  failed.
- `test-e2e.yaml` runs the end-to-end tests on a kind, a minikube and an IPv6 kind cluster.
- `ci` (the `ci-success` job) is the only required check. It only collects the jobs it needs, so when it fails, read
  the job under it that failed.
- `main` merges through the merge queue after one approval and with every review thread resolved, and the queue
  squashes each pull request into one commit.
- nextest retries a failing test twice, so `FLAKY` marks a pass on a retry and `TRY 3 FAIL` a test that never passed.
- Clippy runs for macOS, Windows, and Linux targets, so code behind a `cfg` can fail on one of them only.
- `bazel run //:lint` runs `typos` on the tracked Markdown outside `docs/`, `guides/` and `skills/`, which keep their
  findings until a cleanup, and Vale on the Markdown at the root and every fragment in `changelog.d`. `CONTRIBUTING.md`
  says how to clear a false positive from either.
- `no-greptile` skips Greptile's review, and `no-qodo` skips Qodo's only if it's on before the pull request leaves
  draft (`.pr_agent.toml`).

## Changelog Fragments

Every pull request adds a fragment to `changelog.d/`, or `towncrier check` fails it; `CONTRIBUTING.md`'s Changelog Entry
says which pull requests need none. Name it `<issue>.<type>.md` after an issue in this repository, or
`+<name>.<type>.md` when there's none; never use the ID of a private tracker. The types are the
`[[tool.towncrier.type]]` directories in `towncrier.toml`: `internal` is for what users never see, such as tests and CI,
and any type besides `fixed`, `security`, and `internal` makes the next release a minor one.

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

- `docs/` and `guides/` came from `metalbear-co/docs`, which keeps their earlier history. They document user-visible
  behavior, so a change users notice usually updates them in the same pull request. The configuration reference is the
  exception: it isn't in `docs/`, but is generated from `mirrord-config`'s doc comments (`mirrord/config/AGENTS.md`), so
  a config change updates those instead. GitBook builds the site from the `docs-published` and `guides-published`
  branches, which every mirrord release updates from the released commit (`publish-docs.yaml`), so an edit reaches the
  site with the next release. `mirrord mcp` embeds `docs/docs` when it's built, so an edit there also reaches it with
  the next release. `docs/AGENTS.md` says how to publish a fix between releases.
- `skills/` came from `metalbear-co/skills`, which keeps its earlier history. `mirrord mcp` embeds `skills/skills` when
  it's built (`mirrord/mcp/AGENTS.md`), so an edit reaches it with the next mirrord release. Nothing copies `skills/` to
  `metalbear-co/skills` yet, so an edit doesn't reach that repository.
- `metalbear-co/mirrord-vscode` and `metalbear-co/mirrord-intellij` drive the CLI; `ci.yaml` runs their end-to-end
  tests on release branches.
- `metalbear-co/test-images` builds the app images the end-to-end tests deploy.

## Generated Files

`mirrord-schema.json` and `mirrord-up-schema.json` are generated, as `mirrord/config/AGENTS.md` and
`mirrord/up/AGENTS.md` say. The `skills-fence-tags` check of `bazel run //:lint` validates the config examples in
`skills/` against them, so renaming or removing an option they use fails it.

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
`mirrord/cli/BUILD.bazel`. Debug builds are hit hardest: a function's frame keeps a slot for every future it builds
before awaiting it, so one `match` with a large future per arm pays for all of them.

- Put `#[mirrord_macros::boxed_future]` (above `#[tracing::instrument]`) on an `async fn` with a large future, such as
  a subcommand or a connection step: it builds its future on the heap, and callers keep writing `f(..).await`.
  `Box::pin(f(..))` and `.boxed()` don't help in a debug build, since `f(..)` is still built in the caller's frame
  first.
- On a stack overflow, the Windows CLI prints every frame with its size, largest first. `bazel test //...` on Windows
  runs `//mirrord/cli:win_stack_check`, which fails when the CLI outgrows its budget and shows that report.
- `bazel run //mirrord/cli:win_stack_report` measures peak stack and large frames locally.

## Style Guideline

- Keep imports at file top (no function-local `use`).
- Prefer `to_owned` for `&str` → `String`.
- Always use `foo.rs` instead of `foo/mod.rs` for module roots.
- Run styling commands after every edit.
- `STYLE.md` has more conventions, for Rust and Starlark; follow it too.
