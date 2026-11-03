---
title: Using mirrord with Cloud Agents
description: Let cloud agents such as Cursor cloud agents, Codex cloud, Claude Code on the web, and Devin run and test code against your staging cluster with mirrord, without a seat for each session.
draft: false
toc: true
tags: ["enterprise"]
---

# Using mirrord with Cloud Agents

Many coding agents run in the cloud, not on a developer's machine. Examples are Cursor cloud agents, Codex cloud, Claude Code on the web, Devin, and agent pods that your platform team runs. These agents can use mirrord to run and test their changes against your staging cluster, the same way an agent on a laptop does.

A cloud agent needs a different identity from a developer. By default, mirrord authenticates to the Operator with a user certificate that it creates for each machine. A cloud agent usually starts in a new, short-lived environment for each task. Each environment then registers as a new user and takes up a seat.

To prevent this, give your cloud agents a **CI API key**. Sessions that start with a CI API key don't take up seats. They are machine sessions, and they count towards the concurrent machine sessions of your [plan](https://metalbear.com/mirrord/pricing/).

## What you need

- The mirrord Operator in the cluster, on the Enterprise plan. You need the Enterprise plan to create a CI API key.
- mirrord CLI version `3.195.0` or later in the agent environment.
- A Linux or macOS agent environment. Sessions that use a CI API key don't support Windows.
- Network access from the agent environment to the Kubernetes API of your cluster.

## 1. Give the agent environment access to the cluster

The agent environment needs a [kubeconfig](https://kubernetes.io/docs/concepts/configuration/organize-cluster-access-kubeconfig/) that points to your staging cluster. We recommend that you create a Kubernetes [service account](https://kubernetes.io/docs/concepts/security/service-accounts/) for your agents, and do not use the credentials of a developer.

Add the kubeconfig to the environment that the agents start from, for example as a file in the agent image.

## 2. Create a CI API key

Run this command from a machine that has access to the cluster:

```sh
mirrord ci api-key
```

Set the key as an environment variable named `MIRRORD_CI_API_KEY` in the agent environment. All your agents can use the same key. The key is a credential, so we recommend that you store it as a secret if your agent platform supports secrets.

## 3. Install mirrord and tell the agent when to use it

Install the mirrord CLI in the environment that the agents start from. Then tell the agent when to run its code with mirrord, the same way you do for an agent on a laptop:

- Install the [mirrord agent skills](./ai-skills-plugin.md).
- Generate an `AGENTS.md` file with the [meta-prompt](./the-meta-prompt.md).
- Point the agent at [metalbear.com/agents.md](https://metalbear.com/agents.md).

The agent then runs mirrord commands as it needs them. When `MIRRORD_CI_API_KEY` is set, every session that the agent starts uses the key, and you don't have to change the commands. mirrord runs `mirrord exec` as a CI session in the foreground. For the `mirrord ci` commands, see [CI/CD Integration](../use-cases/mirrord-for-ci.md).

## 4. Keep agent sessions apart

Many agent sessions can target the same service at the same time, as long as each session has a different session key. The session key is not the CI API key. It is a name for one mirrord session, for example the ID of the agent task. Set it in the `MIRRORD_KEY` environment variable of the agent environment, or pass it with `--key`.

Then use the session key so that each session gets only its own traffic and data:

- **Incoming traffic:** Use steal mode with an [HTTP filter](../using-mirrord/incoming-traffic/filter-incoming-traffic.md) on a header that carries the session key. Without a filter, one agent session takes all the traffic of the target.
- **Queues:** Use [queue splitting](../sharing-the-cluster/queue-splitting.md) with a filter on the session key, so that each session gets only its own messages.
- **Databases:** If you don't want all the agents to write to the same database, use [DB branching](../sharing-the-cluster/db-branching.md). Each session then writes to its own branch.

Put the filters in a `mirrord.json` file in the repository, and refer to the session key with `{{ key }}`. The header filter is a regular expression, so anchor it at both ends. Without the anchors, the filter for the key `task-1` also matches `task-10`. Use keys that contain only letters, numbers, and dashes. For example:

```json
{
  "feature": {
    "network": {
      "incoming": {
        "mode": "steal",
        "http_filter": {
          "header_filter": "^x-agent-task: {{ key }}$"
        }
      }
    }
  }
}
```

If the agent runs several services together, use [`mirrord up`](../using-mirrord/multiple-concurrent-sessions.md) with the same session key, for example `mirrord up --key "$AGENT_TASK_ID"`. mirrord makes the HTTP filter and the queue filter of each service from the key. Give the `--key` flag every time. Without it, `mirrord up` uses the OS username as the key. Agent environments usually all have the same username, so all the agents get the same key and receive each other's traffic and messages.
