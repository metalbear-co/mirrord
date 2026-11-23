# `mirrord-mcp`

## Overview

`mirrord-mcp` is the MCP server behind `mirrord mcp`, which exposes mirrord to AI agents over stdio.

Tools are registered in the `tools` module and must be answered offline, from what is compiled into the binary. Usage
is reported through `mirrord-analytics`, and every new tool needs a variant in the `McpTool` telemetry enum.

The docs and skills it answers from are `docs/docs` and `skills/skills` at the repository root, which `build.rs` packs
into the binary, so a build serves those of the checkout it is built from. Change them there. The tests in
`src/corpus.rs` are the checks every page and skill has to pass to ship, and `content_check` in `ci.yaml` runs them
whenever `docs/` or `skills/` change.
