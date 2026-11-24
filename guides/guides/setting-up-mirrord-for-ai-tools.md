# How to Set Up AI Tools with mirrord

In this guide, we'll cover how to configure mirrord with Cursor, Claude Code, GitHub Copilot, Codex, and Windsurf. Each tool reads instructions differently, but the setup is the same: a mirrord config, an instructions file, and optionally a helper script.

---

**Tip:** You can use [mirrord](https://metalbear.com/mirrord/) to test AI-generated code locally with Kubernetes context, without needing to build or deploy each time. If you're new to mirrord, start with the [Quick Start](https://metalbear.com/mirrord/docs/getting-started/quick-start).

---

![Where mirrord fits in the AI Dev Stack](setting-up-mirrord-for-ai-tools/ai-stack.png)

## Common setup: mirrord config

Before configuring any AI tool, create a mirrord config at `.mirrord/mirrord.json`:

```json
{
  "target": {
    "namespace": "your-namespace",
    "path": {
      "deployment": "your-service"
    }
  },
  "feature": {
    "network": {
      "incoming": {
        "mode": "steal",
        "http_filter": {
          "header_filter": "X-Test-Agent: dev"
        }
      }
    }
  }
}
```

For repos with multiple services, create one config per service: `.mirrord/mirrord-<service>.json`.

**Tip:** You can auto-generate configs and agent instructions for your entire repo using the [mirrord skills package](https://github.com/metalbear-co/skills) for Claude Code, or the meta-prompt in [Using mirrord with AI Agents](https://metalbear.com/mirrord/docs/using-mirrord-with-ai) for any tool. The sections below show what to set up manually if you prefer.

## Claude Code

Claude Code reads instructions from `CLAUDE.md` at the repo root. It also reads `AGENTS.md` if present.

### Fast path: install mirrord skills

MetalBear publishes a [skills package](https://github.com/metalbear-co/skills) that gives Claude Code mirrord capabilities out of the box, config generation, quickstart, CI setup, operator installation, and database branching:

```bash
/plugin marketplace add metalbear-co/skills
```

Or with npx:

```bash
npx skills add metalbear-co/skills
```

Once installed, Claude Code can generate mirrord configs, run services against staging, and set up your repo without manual instructions. If you want more control over the instructions, create a `CLAUDE.md` manually instead.

### Manual setup

Create `CLAUDE.md`:

```markdown
# Agent Instructions

## Testing with mirrord

You MUST test code changes against the staging cluster using mirrord before
opening a PR. Do NOT rely on mocks or assume code works without testing.

### Running the service locally with staging context:

mirrord exec --config-file .mirrord/mirrord.json -- <start command>

### Running tests against staging:

mirrord exec --config-file .mirrord/mirrord.json -- <test command>

### Workflow:
1. Make code changes
2. Start the service with mirrord
3. Verify the change works (send requests, run tests)
4. If tests fail, fix and re-run
5. NEVER open a PR without testing against staging
```

### Usage

```console
> Read AGENTS.md. I changed the /orders endpoint to include a discount field.
  Test it against staging.

Claude Code: I'll start the service with mirrord and test your change...
$ mirrord exec --config-file .mirrord/mirrord.json -- npm start
$ curl -H "X-Test-Agent: dev" http://localhost:3000/orders/42
{"orderId": 42, "status": "confirmed", "discount_cents": 150}
OK: The discount field is present and calculated correctly.
```

## Cursor

Cursor reads project-level instructions from `.cursor/rules/` directory. Each file in this directory is loaded as a rule.

### Setup

Create `.cursor/rules/mirrord.mdc`:

```yaml
---
description: Testing code changes against Kubernetes staging using mirrord
globs:
---

You are working in a repository that uses mirrord to test code against a
Kubernetes staging cluster.

RULES:
- ALWAYS test code changes against staging using mirrord before suggesting
  they are complete
- Use this command to run the service with staging context:
  mirrord exec --config-file .mirrord/mirrord.json -- <start command>
- Use this command to run tests against staging:
  mirrord exec --config-file .mirrord/mirrord.json -- <test command>
- If a test fails, read the error output and fix the issue before retrying
- NEVER mark a task as done without testing against real infrastructure

SERVICE CONFIGS:
- Default: .mirrord/mirrord.json
- For specific services, check .mirrord/mirrord-<service>.json

VERIFICATION:
After starting with mirrord, verify the service is running:
  curl -H "X-Test-Agent: dev" http://localhost:<port>/health
```

### Using with Cursor Agent

When using Cursor's Agent mode for multi-file changes, Cursor reads rules from `.cursor/rules/` automatically. Ask it to test:

```console
Make the change and test it with mirrord against staging.
```

Cursor will generate the code, run `mirrord exec`, and verify the result.

## GitHub Copilot

GitHub Copilot and the Copilot Coding Agent read instructions from `.github/copilot-instructions.md`.

### Setup

Create `.github/copilot-instructions.md`:

```markdown
## Testing

This project uses mirrord to test against a live Kubernetes staging cluster.

When making code changes:
1. Run the service with mirrord:
   
   mirrord exec --config-file .mirrord/mirrord.json -- <start command>
   
2. Verify the change works by sending test requests
3. Run the test suite through mirrord:
   
   mirrord exec --config-file .mirrord/mirrord.json -- <test command>

The mirrord config at `.mirrord/mirrord.json` targets the staging deployment.
Traffic filtering ensures only your test requests are intercepted.
```

## Codex

OpenAI Codex reads from `AGENTS.md`.

### Setup

Use the same `AGENTS.md` format as [Claude Code](#claude-code) above. Codex follows the same conventions.

## Windsurf

Windsurf reads from `.windsurfrules`.

### Setup

Create `.windsurfrules`:

```markdown
This project uses mirrord to connect local processes to a Kubernetes staging
cluster for testing.

Testing workflow:
1. Run with mirrord: mirrord exec --config-file .mirrord/mirrord.json -- <cmd>
2. Test the change against real services
3. Fix any failures before completing the task

mirrord configs are in .mirrord/, one per service.
```

## Using the meta-prompt for any tool

Instead of writing configs manually, you can paste the meta-prompt from [Using mirrord with AI Agents](https://metalbear.com/mirrord/docs/using-mirrord-with-ai) into any AI tool. It will:

1. Scan your repo to discover services
2. Ask you which deployments to target
3. Generate `.mirrord/mirrord-<service>.json` configs
4. Generate an `AGENTS.md` with instructions for your specific setup

This works with Claude Code, Cursor, Copilot CLI, and Gemini CLI.

## IDE plugin integration

If you use mirrord's IDE plugins (VS Code or JetBrains), your AI agent's code runs with mirrord context automatically when you hit Run or Debug, no `mirrord exec` needed.

### VS Code, Cursor, Windsurf, and other forks

The mirrord extension works for VS Code and all its forks like Cursor, Windsurf, etc.

1. Install the [mirrord extension](https://marketplace.visualstudio.com/items?itemName=MetalBear.mirrord)
2. Enable mirrord from the status bar
3. When your AI agent says "run the service", use the Run/Debug button, mirrord is already active

### JetBrains (IntelliJ, GoLand, PyCharm, etc.)

1. Install the [mirrord plugin](https://plugins.jetbrains.com/plugin/19772-mirrord)
2. Enable mirrord and select your config
3. Run/Debug your application, mirrord handles the cluster connection

This is useful when you're pair-programming with an AI agent in the IDE rather than letting it run autonomously via CLI.

## Verifying the setup

After configuring any tool, test the integration:

1. Ask the agent to make a trivial change (e.g., add a log line)
2. Ask it to test the change using mirrord
3. Verify the agent runs `mirrord exec` and checks the output
4. Confirm the agent reports real responses from staging, not mock data

If the agent skips the mirrord step, strengthen the language in your instructions file, use "MUST", "ALWAYS", "NEVER" instead of "should" or "please".

## Next steps

- [How to Test AI-Generated Code with mirrord](testing-ai-generated-code.md): test AI-generated code against your Kubernetes cluster step by step
- [Autonomous AI Workflows with mirrord](running-ai-agents-with-mirrord.md): the full agent loop with E2E guardrails and AGENTS.md setup
- [Using mirrord with AI Agents](https://metalbear.com/mirrord/docs/using-mirrord-with-ai): the meta-prompt and detailed setup reference
