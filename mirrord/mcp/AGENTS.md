# `mirrord-mcp`

## Overview

`mirrord-mcp` is the MCP server behind `mirrord mcp`, which exposes mirrord to AI agents over stdio.

Tools are registered in the `tools` module and must be answered offline, from what is compiled into the binary. Usage
is reported through `mirrord-analytics`, and every new tool needs a variant in the `McpTool` telemetry enum.

The docs and skills it answers from are `docs/docs` and `skills/skills` at the repository root, which `:corpus` in
`BUILD.bazel` packs into the binary, so a build serves those of the checkout it is built from. Change them there.
`docs/BUILD.bazel` and `skills/BUILD.bazel` select the files that it packs. The tests in `src/corpus.rs` are the checks
every page and skill has to pass to ship. CI runs them in `build-test`, or in the `lint` job when a pull request only
changes `docs/`, `guides/` or `skills/`.
