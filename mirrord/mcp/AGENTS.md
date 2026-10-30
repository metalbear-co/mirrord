# `mirrord-mcp`

## Overview

`mirrord-mcp` is the MCP server behind `mirrord mcp`, which exposes mirrord to AI agents over stdio.

Tools are registered in the `tools` module and must be answered offline, from what is compiled into the binary. Usage
is reported through `mirrord-analytics`, and every new tool needs a variant in the `McpTool` telemetry enum.

The docs and skills it answers from are vendored into `corpus/` from pinned upstream commits: never edit them by hand,
bump them with `cargo xtask corpus sync --bump` (the release PR does).
