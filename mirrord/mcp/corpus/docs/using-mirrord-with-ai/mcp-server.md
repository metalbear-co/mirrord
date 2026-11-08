---
title: mirrord MCP Server
description: Register the MCP server built into the mirrord CLI, so that your AI agent checks mirrord configs and reads the mirrord docs of the version you have installed.
tags:
  - oss
  - team
  - enterprise
---

# mirrord MCP Server

The mirrord CLI includes an [MCP](https://modelcontextprotocol.io) server, `mirrord mcp`. It gives an AI agent tools to validate `mirrord.json` and `mirrord-up.yaml` against the schema of your installed mirrord version, to explain config options, and to search and read the mirrord docs and [skills](./ai-skills-plugin.md) that this version ships with. It works offline and never starts a session.

## What you need

- mirrord CLI `3.273.0` or later. Check with `mirrord --version`, and see [Installing the mirrord CLI](../installing-mirrord/cli.md).
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
| `validate_config` | Checks the content of a `mirrord.json` or `mirrord-up.yaml` against the schema of the installed version. Returns each issue with the path of the value, a message, and the allowed values. An empty `issues` list means the config is valid. |
| `explain_config_option` | Explains one option by its dotted path, such as `feature.network.incoming.mode`: what it does, the values it takes, its default, and the mirrord plan it needs. |
| `get_skill` | Lists the mirrord skills, or returns one skill and the files it bundles. |
| `search_docs` | Searches the docs, skills, and config options by keywords, and returns the best matching pages. |
| `read_doc` | Returns the full Markdown of one page or config option that `search_docs` found. Without a path, lists every page. |

## Resources and prompts

- `mirrord://info`: the installed mirrord version and the docs and skills it ships with.
- `mirrord://docs/<path>`: each docs page.
- `mirrord://skills/<name>`: each skill. Each skill is also an MCP prompt.

## What the server tells the agent

The server sends the agent instructions when it connects. They tell the agent to:

- Get the matching skill with `get_skill` before it starts a mirrord task.
- Validate every config with `validate_config` before it writes the file, and fix issues until none remain.
- Answer questions about mirrord with `search_docs` and `read_doc`, not from memory.
- Default to `mirror` for incoming traffic, and on a shared cluster use `steal` only with an HTTP filter.
- Target staging or development clusters, never production.

## Versions

The docs, skills, and config schema come from the installed mirrord binary, so the answers match the version you run. To get newer docs and skills, upgrade mirrord.
