---
title: mirrord MCP Server
description: Register the MCP server built into the mirrord CLI, so that your AI agent generates and validates mirrord configs offline, for the mirrord version you run.
tags:
  - oss
  - team
  - enterprise
---

# mirrord MCP Server

`mirrord mcp` is the [MCP](https://modelcontextprotocol.io) server built into the mirrord CLI. With it, your AI agent:

- Generates `mirrord.json` and `mirrord-up.yaml` files, and validates them against the schema of the mirrord version you run.
- Explains any config option: what it does, the values it takes, and the mirrord plan it needs.
- Follows the mirrord [skills](./ai-skills-plugin.md) for tasks such as database branching, Kafka splitting, preview environments, and CI.
- Uses safe defaults on shared clusters: it mirrors incoming traffic, steals only with an HTTP filter, and targets staging or development clusters, never production.
- Answers mirrord questions from the docs of your installed version.

The server works offline, from the docs and schema built into your mirrord binary, and never starts a session.

## What you need

- mirrord CLI `3.273.0` or later, and `3.291.0` or later on Windows for the skills. Check with `mirrord --version`, and see [Install mirrord](../getting-started/install-mirrord.md).
- An agent that supports MCP servers over stdio, such as Claude Code, Codex, Cursor, VS Code, Windsurf, Claude Desktop, or OpenCode.

## Register the server

Use the absolute path to the `mirrord` binary. Clients that start from a desktop launcher don't inherit your shell's `PATH`. To find the path, run:

```sh
which mirrord
```

On Windows, run `where mirrord`. Replace `/absolute/path/to/mirrord` below with the output.

{% tabs %}
{% tab title="Claude Code" %}

```sh
claude mcp add --scope user mirrord -- /absolute/path/to/mirrord mcp
```

{% endtab %}
{% tab title="Codex" %}

```sh
codex mcp add mirrord -- /absolute/path/to/mirrord mcp
```

{% endtab %}
{% tab title="Cursor, Windsurf, Claude Desktop" %}

Add the entry under `"mcpServers"` in the client's config file:

- Cursor: `~/.cursor/mcp.json`, or `.cursor/mcp.json` in the project.
- Windsurf: `~/.codeium/windsurf/mcp_config.json`.
- Claude Desktop: `claude_desktop_config.json`, in `~/Library/Application Support/Claude/` on macOS and `%APPDATA%\Claude\` on Windows.

```json
"mirrord": { "command": "/absolute/path/to/mirrord", "args": ["mcp"] }
```

{% endtab %}
{% tab title="VS Code" %}

Add the entry under `"servers"` in `.vscode/mcp.json`:

```json
"mirrord": { "type": "stdio", "command": "/absolute/path/to/mirrord", "args": ["mcp"] }
```

{% endtab %}
{% tab title="OpenCode" %}

Add the entry under `"mcp"` in `~/.config/opencode/opencode.json`:

```json
"mirrord": { "type": "local", "command": ["/absolute/path/to/mirrord", "mcp"], "enabled": true }
```

{% endtab %}
{% endtabs %}

Restart the client after you register the server.

To check the connection, ask the agent to validate your `.mirrord/mirrord.json` with the `validate_config` tool. If the client lists no `mirrord` tools, check the path in the entry with `/absolute/path/to/mirrord mcp --help`.

## Tools

| Tool | What it does |
| --- | --- |
| `validate_config` | Checks the content of a `mirrord.json` or `mirrord-up.yaml` against the schema of the installed version. Returns each issue with the path of the value, a message, and the allowed values. An empty `issues` list means the config is valid. Takes the directory you run mirrord from, to resolve a relative protobuf `schema_file`. |
| `explain_config_option` | Explains one option by its dotted path, such as `feature.network.incoming.mode`: what it does, the values it takes, its default, and the mirrord plan it needs. |
| `get_skill` | Lists the mirrord skills, or returns one skill and the files it bundles. |
| `search_docs` | Searches the docs, skills, and config options by keywords, and returns the best matching pages. |
| `read_doc` | Returns the full Markdown of one page or config option that `search_docs` found. Without a path, lists every page. |

## Resources and prompts

- `mirrord://info`: the installed mirrord version. The docs and skills the server answers from are those of that version.
- `mirrord://docs/<path>`: each docs page.
- `mirrord://skills/<name>`: each skill. Each skill is also an MCP prompt.

## What the server tells the agent

The server sends the agent instructions when it connects. They tell the agent to:

- Get the matching skill with `get_skill` before it starts a mirrord task.
- Validate every config with `validate_config` before it writes the file, and fix issues until none remain.
- Look up options with `explain_config_option` instead of guessing.
- When mirrord fails, validate every config involved before it proposes a fix, and propose one fix at a time.
- Answer questions about mirrord with `search_docs` and `read_doc`, not from memory.
- Default to `mirror` for incoming traffic, and on a shared cluster use `steal` only with an HTTP filter.
- Target staging or development clusters, never production.
- Know that `mirrord exec` and `mirrord up` deploy nothing to the cluster, that database branches stay until their TTL runs out, and that preview environments stay until stopped or their TTL runs out.

## Versions

The docs, skills, and config schema come from the installed mirrord binary, so the answers match the version you run. To get newer docs and skills, upgrade mirrord.
