---
title: Using mirrord with AI Agents
description: Configure AI coding agents to test code changes against your cluster using mirrord
draft: false
toc: true
tags: ["oss", "team", "enterprise"]
---

AI coding models don’t have the full context of your application — external dependencies, environment variables, and the way services interact in production. The code they generate might look correct in isolation, but things often break once you try integrating it with the rest of your system.

If you're using tools like Claude Code, Cursor, or Codex, mirrord lets you safely test AI-generated code in a real, production-like Kubernetes environment in seconds, instead of relying on mocks or waiting for CI/CD to reveal integration issues.

Here are some ways you can connect mirrord to your AI coding workflow:

- **[Agent skills for mirrord](./ai-skills-plugin.md)** - Install skills that give your AI assistant built-in mirrord expertise.
- **[Configure AI Agents to Use mirrord](./the-meta-prompt.md)** - A prompt generator that creates project-specific `AGENTS.md` files and configurations automatically, saving hours of manual setup across multiple services.
- **[metalbear.com/agents.md](https://metalbear.com/agents.md)** - A short operational reference written for agents rather than people: install commands, finding targets, running code through mirrord, configuration, and etiquette on a shared cluster. Point an agent at the URL directly when you want it to pick mirrord up without any repository setup.
- **[Agent-Started Trials](./agent-started-trials.md)** - On a cluster with no mirrord for Teams license, an agent can start a trial itself and hand you a link to claim the organization it created.
