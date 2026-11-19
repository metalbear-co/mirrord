# How to Test AI-Generated Code with mirrord

In this guide, we'll cover how to test AI-generated code against your Kubernetes cluster using mirrord. You'll learn how to run your local process with staging context so you can validate changes in seconds instead of waiting for CI/CD pipelines.

---

**Tip:** You can use mirrord to test AI-generated code locally with Kubernetes context, without needing to build or deploy each time. If you're new to mirrord, start with the [Quick Start](https://metalbear.com/mirrord/docs/getting-started/quick-start).

---

## Prerequisites

- A Kubernetes cluster with your services running (staging or dev)
- [mirrord CLI installed](https://metalbear.com/mirrord/docs/installing-mirrord/cli)
- An AI coding tool (Claude Code, Cursor, Copilot, Codex)

## Step 1: Create a mirrord config

Create `.mirrord/mirrord.json` targeting the service you're working on:

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

The `http_filter` ensures you only intercept traffic with your header, other developers and services using the same deployment are unaffected.

## Step 2: Let AI generate code, then validate immediately

Instead of the build, deploy, test cycle, the workflow becomes:

1. AI generates code changes
2. Run locally with mirrord (substitute your framework's start command and port):
   ```bash
   mirrord exec --config-file .mirrord/mirrord.json -- <start command>
   ```
3. Send a test request:
   ```bash
   curl http://localhost:<port>/your-endpoint
   ```
4. See real responses from real services, no mocks, no deploy

If something is wrong, fix it and re-run.

## Step 3: Run your test suite through mirrord

For automated validation, you have two options depending on your test setup:

**Option A: Run the test runner through mirrord.** This works when your tests start the service themselves (common with frameworks like FastAPI's TestClient or supertest):

```bash
mirrord exec --config-file .mirrord/mirrord.json -- pytest tests/integration/
```

**Option B: Start the service via mirrord, then run tests against localhost.** This works when your tests hit an already-running service (curl-based scripts, Playwright, Cypress):

```bash
# Terminal 1: start the service
mirrord exec --config-file .mirrord/mirrord.json -- <start command>

# Terminal 2: run tests against localhost
./ci/e2e_test.sh
```

In both cases, your tests and service run locally but with access to real dependencies in the cluster.

## Example: validating a new endpoint

An AI agent adds a `/discount` endpoint to an order service. Without mirrord, you'd need to build, deploy, and test against staging. With mirrord:

```bash
# AI generates the code
# Run locally with staging context
mirrord exec --config-file .mirrord/mirrord.json -- <start command>
```

```bash
# Test the new endpoint against real data
curl http://localhost:<port>/discount \
  -H "Content-Type: application/json" \
  -d '{"orderId": 42}'
```

The request reaches your local code, but when your code queries the database or calls other services, those calls go to the real staging cluster. You immediately know:

- Does the database query work with real schema and data?
- Do downstream service calls return what the AI expected?
- Do auth tokens and permissions work correctly?
- Is the response format compatible with the frontend?

## Next steps

- [Autonomous AI Workflows with mirrord](running-ai-agents-with-mirrord.md): the full agent loop with E2E guardrails and AGENTS.md setup
- [How to Set Up AI Tools with mirrord](setting-up-mirrord-for-ai-tools.md): per-tool config for Cursor, Claude Code, Copilot, and Codex
- [Using mirrord with AI Agents](https://metalbear.com/mirrord/docs/using-mirrord-with-ai): auto-generate mirrord configs and AGENTS.md for your repo
