---
name: mirrord-quickstart
description: Guide users from zero to their first working mirrord session — check system requirements, install mirrord via CLI, VS Code, or IntelliJ, run a first session against a Kubernetes target, and verify the connection. Use when a user is new to mirrord, wants to install it, needs help running their first session, or wants to debug or test a local process against their Kubernetes cluster.
metadata:
  author: MetalBear
  version: "1.2"
---

# Mirrord Quickstart Skill

## Purpose

Help new users get mirrord running quickly:
- **Check** system requirements
- **Install** mirrord (CLI, VS Code, or IntelliJ)
- **Connect** to their first Kubernetes target
- **Verify** the connection works

Go one step at a time — don't overwhelm a new user with all options at once, and when they first connect, explain what just happened.

## Critical First Steps

**Step 1: Detect user's environment**
Ask or detect:
- Operating system (macOS, Linux, Windows)
- Preferred workflow (CLI, VS Code, IntelliJ)
- Do they have kubectl configured?

**Step 2: Verify requirements**
```bash
# Check kubectl access
kubectl cluster-info
kubectl get pods -A | head -5
```

If kubectl fails, help them configure it first.

## Installation Paths

### CLI (recommended for getting started)

**Do not** run remote install scripts that pipe a network download into a shell interpreter. Use only methods your organization approves.

**Official guide:** Follow [mirrord installation documentation](https://mirrord.dev/docs/overview/quick-start/) for supported options (package managers, pinned release binaries with checksum verification, etc.).

**Summary for the agent:**
- **macOS / Linux:** Point the user to the official docs for Homebrew, apt, or pinned binary install steps — do not invent or paste one-liners that fetch and execute remote scripts.
- **Windows:** Point the user to the official docs for supported installers.

> **Security:** Prefer package managers or manually verified binaries from official release artifacts. Never execute installation by piping downloaded content into a shell.

**Verify installation:**
```bash
mirrord --version
```

### VS Code Extension

1. Open VS Code Extensions (Cmd/Ctrl+Shift+X)
2. Search "mirrord"
3. Install "mirrord" by MetalBear
4. Look for mirrord icon in status bar

### IntelliJ Plugin

1. Open Settings → Plugins → Marketplace
2. Search "mirrord"
3. Install and restart IDE
4. Find mirrord in navigation toolbar

## First Session

### CLI approach

```bash
# List available targets
mirrord ls

# Run a local process with mirrord
mirrord exec --target pod/<pod-name> -- <your-command>

# Example: Node.js app
mirrord exec --target pod/api-server-7c8d9 -- node app.js

# Example: Python app
mirrord exec --target pod/backend-abc123 -- python main.py
```

If `mirrord ls` returns no targets, check that the kubeconfig context points at the right cluster (`kubectl config current-context`) and list pods in the expected namespace (`kubectl get pods -n <namespace>`).

### IDE approach

1. Enable mirrord (click status bar icon / toolbar button)
2. Select target pod when prompted
3. Run/debug your application normally
4. mirrord intercepts and connects automatically

## Verification

After running, verify the connection:

1. **Check logs** - mirrord's startup progress (agent started, connected to the target) prints before your app's own output:
```bash
mirrord exec --target pod/<pod-name> -- <your-command>
```
2. **Test environment** - Remote env vars should be available locally:
```bash
mirrord exec --target pod/<pod-name> -- env | grep -i database
```
3. **Test network** - Send a request to the remote service; it should reach the local process (check the local process's output):
```bash
curl http://<remote-service-url>
```

## What's Next

Once the first session works, point users to:
- **`mirrord ui`** — launches a local dashboard showing every active session on the machine, with a live event stream (HTTP requests, file operations, DNS, outgoing connections). See [Local UI](https://metalbear.com/mirrord/docs/using-mirrord/local-ui).
- **`mirrord up`** — for debugging several microservices together from one `mirrord-up.yaml` (compose-style). Point to the **mirrord-up** skill.
- **Browser extension** — routes requests made from Chrome to the local process, for testing an HTTP-filtered session from a browser instead of `curl`. See [Debugging from Browser](https://metalbear.com/mirrord/docs/using-mirrord/incoming-traffic/debug-from-browser).
- **mirrord for Teams features** (DB branching, queue splitting, preview environments, concurrent steal on one target) need the Operator. On a cluster with no license, an AI agent can offer the user a seven-day Enterprise trial and start it once they agree: point to the **mirrord-operator** skill, "Agent-started trial".

## Common First-Timer Issues

| Issue | Solution |
|-------|----------|
| "kubectl not found" | Install kubectl and configure cluster access |
| "No pods found" | Check namespace: `kubectl get pods -n <namespace>` |
| "Permission denied" | Check RBAC permissions for your kubectl context |
| "Agent failed to start" | Ensure cluster runs Linux kernel 4.20+ |
