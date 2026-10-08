---
title: Configure AI Agents to Use mirrord
description: Configure AI coding agents to test code changes against your cluster using mirrord
draft: false
toc: true
tags: ["oss", "team", "enterprise"]
---

Agents can even do this autonomously. By instructing them to use mirrord as part of their workflow, they can run code against a real environment, catch issues early, and iterate until the feature actually works end-to-end. They can even use mirrord to explore the staging environment before writing code, giving them better visibility into how things are configured and service dependencies so they write more accurate code to begin with.

The workflow: the agent writes code, runs it with mirrord against the cluster, checks the result, and iterates. Feedback cycles drop from minutes to seconds.

## How it works

Most AI coding agents support an instructions file (e.g. `AGENTS.md`, `.cursorrules`) that tells the agent how to work in your repository. By adding mirrord instructions to this file, the agent automatically uses mirrord when testing changes.

The setup consists of three pieces:

1. **A mirrord config file** per service (`.mirrord/mirrord-<service>.json`) specifying the Kubernetes target, namespace, and traffic mode
2. **A helper script** per service (`scripts/mirrord-<service>.sh`) that wraps the mirrord command with pre-flight checks
3. **An `AGENTS.md` file** that tells the agent to use mirrord and references the helper scripts

## Generating the setup with a meta-prompt

Rather than writing these files manually, you can use the following prompt with any AI coding agent. Paste it at the repository root and the agent will discover your services, ask you a series of questions, and generate everything.

```
Generate AGENTS.md with mirrord integration for this repository.

Discovery Phase:
1. Scan repository structure to identify services (main.go, app.py, server.js, etc.)
2. Detect programming languages and frameworks
3. Look for Kubernetes manifests or conventions to understand deployment structure
4. Identify potential Kubernetes targets for each service (do NOT assume correctness)
5. Present discovered services and suspected targets in a table, including:
   - Any existing mirrord configuration files (show path and target)
   - Confidence level for each suspected target

STOP — Ask ONE question at a time. WAIT for my answer before continuing.

Question 1:
"Which services should be configured for AI-driven testing?"
Show the discovered services and WAIT FOR MY ANSWER.
If a service has existing config, note: "(has config - will ask to keep/replace)"

For each selected service, ask the following questions ONE AT A TIME.
WAIT for my answer before proceeding to the next question.

Question 2:
"For service <name>: What Kubernetes target should be used?"

Based on discovery, here are possible targets for this service:
- <candidate target 1> (target type, namespace, confidence)
- <candidate target 2> (target type, namespace, confidence)

Please select one of the above OR provide a different target.

Provide:
- Target type: deployment or pod
- Target name
- Namespace

DO NOT assume a default.
WAIT FOR MY ANSWER.

Question 3:
"For service <name>: How should testing success be verified when running with mirrord?"

Choose one:
- HTTP behavior (I will provide details such as domains/paths..)
- Logs (I will provide success message)
- Side effects (I will describe what to check)
- No automated verification

WAIT FOR MY ANSWER.

Question 4 (if needed, based on verification method):
Ask for verification coordinates only if required.
- Domains, paths, and headers may be supplied by me
- OR explicitly marked as environment variables
- Do NOT guess or hard-code remote URLs
WAIT FOR MY ANSWER.

Question 5:
"For service <name>: Which mirrord network mode should be used?"
- steal (intercept traffic)
- mirror (observe traffic)
WAIT FOR MY ANSWER.

Question 6:
"For service <name>: Any special requirements?"
- Filesystem access
- Environment variables
- Traffic filtering
WAIT FOR MY ANSWER.

IMPORTANT:
- Ask ONE question at a time
- WAIT for my answer before continuing
- DO NOT assume defaults
- DO NOT generate files yet

Generation Phase (ONLY after all required answers are provided):
For each configured service, generate:

1. `.mirrord/mirrord-<service>.json`
   - Use the confirmed target type, target name, and namespace
   - Configure the selected network mode
   - Apply traffic filtering ONLY if I explicitly provided it
   - Use only fields supported by the installed mirrord version
   - Avoid embedding secrets or sensitive values

2. `scripts/mirrord-<service>.sh` helper script
   - Perform pre-flight checks (mirrord installed, kubectl access, target exists)
   - Execute mirrord safely
   - Do NOT echo sensitive values to output
   - Include a --stop flag to kill running mirrord session

3. `AGENTS.md`
   - Start with a prominent ATTENTION block
   - Use strong imperative language (MUST / NEVER / ALWAYS)
   - Define mirrord as the required testing runtime
   - Describe verification behavior, not guessed URLs or credentials
   - Reference helper scripts instead of inline commands where possible
   - Include a service overview table
   - Add a troubleshooting section

Validation:
- Verify mirrord is installed (`mirrord --version`)
- Check kubectl access (`kubectl config current-context`)
- Validate each configured target exists in the specified namespace
- Ensure helper scripts are executable

Report Back:
- Services discovered
- Services selected for configuration
- Confirmed target and namespace per service
- Verification method per service
- Files created (paths only)
- Any remaining manual inputs or values requiring review
```

### Demo

Here's the meta-prompt in action using the [MetalBear playground repository](https://github.com/metalbear-co/playground):

{% embed url="https://www.youtube.com/watch?v=EPMTFWQJy4M" %}

## What gets generated

The meta-prompt produces three types of files:

### `AGENTS.md`

The instructions file uses strong imperative language ("MUST", "NEVER", "ALWAYS") because AI agents respond more reliably to direct instructions than soft suggestions. It includes exact testing commands and references the helper scripts.

### `.mirrord/mirrord-<service>.json`

A mirrord configuration per service, specifying the Kubernetes deployment target, namespace, traffic mode (steal/mirror), and any header filters.

### `scripts/mirrord-<service>.sh`

A wrapper script that runs pre-flight checks before executing mirrord: verifies mirrord is installed, confirms kubectl access, checks the deployment exists, and prints the active traffic filter.

## Testing with the generated setup

Once the files are generated, tell your AI agent to read `AGENTS.md` and test a service:

```
Read AGENTS.md and test the ip-visit-counter service. Send 3 requests.
```

The agent will run the helper script, send requests through the cluster, and verify the results.

{% embed url="https://www.youtube.com/watch?v=1G046UqS9gE" %}

You can also make code changes and test them immediately:

```
Read AGENTS.md. Change the ip-visit-counter response to include a "message": "Welcome!" field and a "date" field with the current date. Then test the changes by sending 2 requests to the /count endpoint and show me the responses.
```

{% embed url="https://www.youtube.com/watch?v=TdqKUXdvR0M" %}

## Compatibility

This workflow has been tested with **Claude Code, Cursor, GitHub Copilot CLI, and Gemini CLI**. All followed the same interactive, step-by-step process. If you're using a different assistant, you may need to run the discovery and generation steps as separate prompts.

## Keeping it updated

As your services evolve, re-run the meta-prompt. New services get new configs and scripts. Renamed deployments update the targets automatically. The generated `AGENTS.md` is a starting point. Add sections for your testing conventions, internal docs, or specific scenarios as needed.

{% hint style="info" %}
**Safety note**

When working with AI agents and live Kubernetes environments:

- Keep your AI assistant in approval mode so you can review changes before they run
- Never target production clusters. Use staging or development environments only
- Start with one service at a time until you're comfortable with the workflow
  {% endhint %}
