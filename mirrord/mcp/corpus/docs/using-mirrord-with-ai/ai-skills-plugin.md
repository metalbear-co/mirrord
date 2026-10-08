---
title: Agent Skills for mirrord
tags:
  - oss
  - team
  - enterprise
description: "Install the mirrord skills plugin to give AI coding assistants like Cursor and Claude Code domain-specific knowledge of how to work with mirrord."
---

MetalBear maintains a **mirrord skills plugin** that extends AI coding assistants, such as Cursor and Claude Code with domain-specific knowledge about mirrord. Instead of relying solely on general instructions in `AGENTS.md`, these skills give your AI assistant built-in expertise for mirrord configuration, troubleshooting, and best practices.

{% embed url="https://www.youtube.com/watch?v=zbbJaorZYl0" %}

**See the mirrord agent skills plugin in action**

## What Are mirrord Skills?

Skills are reusable instruction modules that teach AI agents how to work with mirrord. When you install the mirrord skills plugin, your AI assistant can:

- Generate and validate `mirrord.json` configuration files
- Guide you through installation and your first mirrord session
- Help set up mirrord in CI pipelines
- Configure the mirrord operator for team environments
- Help set up [database branching](../sharing-the-cluster/db-branching.md) for your cluster
- Configure [queue splitting](../sharing-the-cluster/queue-splitting.md) for Kafka topics
- Create [preview environments](../use-cases/preview-environments.md), ad hoc or per-PR in CI
- [Chaos test](../use-cases/chaos-testing.md) your app with per-session latency and connection-error rules


## Available Skills

For the full list of skills, what each one covers, and example prompts, see <a href="https://github.com/metalbear-co/skills/#mirrord-agent-skills" target="_blank" rel="noopener noreferrer">mirrord Agent Skills</a> in the <a href="https://github.com/metalbear-co/skills/" target="_blank" rel="noopener noreferrer">metalbear-co/skills</a> repository.

## Installing the mirrord Skills

The mirrord skills are distributed as a plugin that you install into your AI coding assistant. Installation steps depend on your tool.

Check the <a href="https://github.com/metalbear-co/skills/" target="_blank" rel="noopener noreferrer">mirrord skills repository</a> for the latest installation instructions...

## Agents That Don't Support Agent Skills

GitHub Copilot and Cline read repository rules files instead of Agent Skills. The skills repository ships drop-in equivalents carrying the same core content in its <a href="https://github.com/metalbear-co/skills/tree/main/ports" target="_blank" rel="noopener noreferrer">ports directory</a>: copy `ports/github-copilot/copilot-instructions.md` into your repository's `.github/` folder, or `ports/cline/.clinerules` into the repository root.
