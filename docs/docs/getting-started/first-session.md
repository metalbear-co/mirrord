---
title: Starting your first session
description: "Run your local code in the context of your Kubernetes cluster or Amazon ECS with the mirrord CLI, and see what mirrord does by default."
tags:
  - oss
  - team
  - enterprise
---

# Starting your first session

You need [mirrord installed](install-mirrord.md) and a workload to run against: a deployment or a pod in your cluster, [another target](../reference/targets.md), or a [workload outside Kubernetes](../using-mirrord/beyond-kubernetes/README.md), such as an Amazon ECS task.

Using VS Code or a JetBrains IDE? See [Use mirrord in your IDE](use-mirrord-in-your-ide.md).

## CLI

To run a local process in the context of the remote target:

```bash
mirrord exec --target <target-path> <command to run locally>
```

For example, to run a Python app as if it were the remote pod:

```bash
mirrord exec --target pod/app-pod-01 python main.py
```

On macOS or Linux, you can run a local container instead of a native process:

```bash
mirrord container --target pod/app-pod-01 -- docker run nginx
```

Use `mirrord exec --help` or `mirrord container --help` for all options.

## Multiple services

To run several services together from one config file, use [`mirrord up`](../using-mirrord/multiple-concurrent-sessions.md):

```bash
mirrord up init
```

This walks you through an interactive wizard and writes a `mirrord-up.yaml` for you, so you don't need to write any config by hand. To answer the same questions in your browser, run `mirrord wizard` (see [Onboarding Wizard](onboarding-wizard.md)). Once you have a `mirrord-up.yaml`, start the services by running `mirrord up`.

`mirrord up init` and the wizard find Kubernetes workloads only. For a workload outside Kubernetes, write its `serverless/` target into `mirrord-up.yaml` yourself. See [Using mirrord with a Workload Outside Kubernetes](../using-mirrord/beyond-kubernetes/README.md#using-mirrord-with-a-workload-outside-kubernetes).

{% hint style="success" %}
**Send a request to your remote target.** You should see it arrive at your local process as well!
{% endhint %}

## What just happened?

By default, mirrord does the following:

| Feature | Default behavior |
|---------|-----------------|
| **Incoming traffic** | Mirrored: your local process receives a copy of traffic hitting the remote pod |
| **Outgoing traffic** | Tunneled through the remote pod, so your local process can access cluster-internal services |
| **Environment variables** | Imported from the remote pod into your local process |
| **File reads** | Read from the remote pod's filesystem |
| **DNS** | Resolved on the remote pod |

Your remote pod keeps running normally, and nothing is disrupted.

## Watch your sessions

Launch the local dashboard to see your mirrord sessions live:

```bash
mirrord ui
```

It opens in your browser and shows every active session on your machine, with a live event stream (HTTP requests, file operations, DNS, outgoing connections). With the mirrord Operator installed, it also shows your teammates' sessions across the cluster. See [Local UI](../using-mirrord/local-ui.md).

## Test from your browser

The mirrord browser extension for Chrome, Edge, and Firefox routes requests you make from your browser to your local process, so you can hit a staging URL and have your local code answer. It works with a session that steals traffic with an [HTTP filter](../using-mirrord/incoming-traffic/filter-incoming-traffic.md). See [Debugging from Browser](../using-mirrord/incoming-traffic/debug-from-browser.md) to install and set it up.

{% hint style="info" %}
**Working with a team?** [mirrord for Teams](https://app.metalbear.com) adds access control, traffic policies, and concurrent session management so your whole team can use mirrord safely.

**Working with an AI coding agent?** It can start a seven-day Enterprise trial itself, without an account or a credit card, and hand you a link to claim it afterwards. See [Agent-Started Trials](../using-mirrord-with-ai/agent-started-trials.md).
{% endhint %}

## Configuration

mirrord reads config from `<project-path>/.mirrord/mirrord.json` (also supports `.toml` and `.yaml`). You can also prefix config files, e.g. `my-config.mirrord.json`.

See the full [configuration options](https://metalbear.com/mirrord/docs/config). To keep an Operator preference across projects, see [Global Configuration](../installing-mirrord/global-configuration.md).

## Next steps

**What are you trying to do?**

| Goal | Guide |
|------|-------|
| **Test against live traffic** | [Steal incoming traffic](../using-mirrord/incoming-traffic/filter-incoming-traffic.md) so your local process responds to real requests instead of the remote pod |
| **Debug several microservices together** | [`mirrord up`](../using-mirrord/multiple-concurrent-sessions.md) runs multiple sessions from a single config file and manages their lifecycle as one |
| **Test through your browser** | The [browser extension](../using-mirrord/incoming-traffic/debug-from-browser.md) routes requests from Chrome to your local process, no proxy setup needed |
| **Debug a queue consumer** | [Queue splitting](../sharing-the-cluster/queue-splitting.md) lets your local process consume messages without competing with the deployed service |
| **Run a tool in cluster context** | [Targetless mode](../using-mirrord/targetless.md) lets you run scripts or tools with cluster network access, without impersonating a specific pod |
| **Use mirrord with an AI agent** | [Set up Claude Code, Cursor, or Codex](../using-mirrord-with-ai/) to test generated code against your cluster automatically |
| **Set up for your team** | [Install the mirrord Operator](install-the-operator.md) for access control, policies, and multi-user support |

Need help or want to share feedback? [Join our Slack community](https://metalbear.com/slack)
