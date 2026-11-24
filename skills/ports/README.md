# Ports: mirrord rules for agents that don't consume Agent Skills

The thirteen skills in this repo are the richest way to teach an agent mirrord. If your agent consumes [Agent Skills](https://agentskills.io) (Claude Code, Codex, OpenCode, Cursor, Gemini CLI, and others), use those — see the [main README](../README.md).

These ports carry the same core content as drop-in rules files for agents that don't:

| Agent | File | Where to put it |
|-------|------|-----------------|
| GitHub Copilot | [`github-copilot/copilot-instructions.md`](github-copilot/copilot-instructions.md) | `.github/copilot-instructions.md` in your repo |
| Cline | [`cline/.clinerules`](cline/.clinerules) | `.clinerules` in your repo root |

Copy the file into your repository and the agent picks it up automatically. The content is intentionally compact — rules files load into every prompt — and links to [metalbear.com/agents.md](https://metalbear.com/agents.md) for the fuller operational guide.

Want another format (Windsurf, Zed, JetBrains Junie, ...)? [Open an issue](https://github.com/metalbear-co/skills/issues).
