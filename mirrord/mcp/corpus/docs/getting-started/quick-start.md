---
title: Quick Start
date: 2020-11-16T12:59:39.000Z
lastmod: 2020-11-16T12:59:39.000Z
draft: false
images: []
menu:
  docs:
    parent: overview
weight: 105
toc: true
tags:
  - oss
  - team
  - enterprise
shallowToc: true
description: How to (very) quickly start using mirrord
---

Get mirrord running in under 5 minutes. You'll need:

- **Locally:** macOS (Intel/Apple Silicon), Linux (x86_64), or Windows (x86_64/WSL). `kubectl` configured and pointing at your cluster.
- **In the cluster:** A running workload (deployment, pod, [etc.](../reference/targets.md)) you want to work with. Linux kernel 4.20+, Docker or containerd runtime.

## Install

{% tabs %}
{% tab title="CLI (macOS/Linux)" %}

```bash
brew install metalbear-co/mirrord/mirrord
```

or:

```bash
curl -fsSL https://raw.githubusercontent.com/metalbear-co/mirrord/main/scripts/install.sh | bash
```

{% endtab %}

{% tab title="CLI (Windows)" %}

```powershell
choco install mirrord
```

{% endtab %}

{% tab title="VS Code / Cursor / Windsurf" %}

Extensions → search **mirrord** → Install.

Works with all VS Code forks (Cursor, Windsurf, Antigravity, PearAI, Trae).

You can also download it from the [VS Code Marketplace](https://marketplace.visualstudio.com/items?itemName=MetalBear.mirrord).

{% endtab %}

{% tab title="JetBrains" %}

Preferences → Plugins → search **mirrord** → Install.

Works with IntelliJ, GoLand, PyCharm, and other JetBrains IDEs.

You can also download it from the [JetBrains Marketplace](https://plugins.jetbrains.com/plugin/19772-mirrord).

{% endtab %}
{% endtabs %}

## Try it

### CLI

To run a local process in the context of the remote target:

```bash
mirrord exec --target <target-path> <command to run locally>
```

For example, to run a Python app as if it were the remote pod:

```bash
mirrord exec --target pod/app-pod-01 python main.py
```

Or run a local container instead of a native process:

```bash
mirrord container --target pod/app-pod-01 -- docker run nginx
```

Use `mirrord exec --help` or `mirrord container --help` for all options.

### Multiple services

To run several services together from one config file (think `docker compose`, but for mirrord), use [`mirrord up`](../using-mirrord/multiple-concurrent-sessions.md):

```bash
mirrord up init
```

This walks you through an interactive wizard and writes a `mirrord-up.yaml` for you, so you don't need to write any config by hand. Once you have a `mirrord-up.yaml`, start the services by running `mirrord up`.

### IDE Extensions

1. **VS Code:** Click **Enable mirrord** in the status bar at the bottom of the window.
2. **JetBrains:** Click the **mirrord icon** in the navigation toolbar (top right).

Then start a debug session. You'll be prompted to select a pod — pick the one you want to impersonate, and your local process will be plugged into it.

{% hint style="success" %}
**Send a request to your remote target** — you should see it arriving at your local process as well!
{% endhint %}

## What just happened?

By default, mirrord does the following:

| Feature | Default behavior |
|---------|-----------------|
| **Incoming traffic** | Mirrored — your local process receives a copy of traffic hitting the remote pod |
| **Outgoing traffic** | Tunneled through the remote pod, so your local process can access cluster-internal services |
| **Environment variables** | Imported from the remote pod into your local process |
| **File reads** | Read from the remote pod's filesystem |
| **DNS** | Resolved on the remote pod |

Your remote pod continues running normally — nothing is disrupted.

## Watch your sessions

Launch the local dashboard to see your mirrord sessions live:

```bash
mirrord ui
```

It opens in your browser and shows every active session on your machine, with a live event stream (HTTP requests, file operations, DNS, outgoing connections). With the mirrord Operator installed, it also shows your teammates' sessions across the cluster. See [Local UI](../using-mirrord/local-ui.md).

## Test from your browser

Install the [mirrord browser extension](https://chromewebstore.google.com/detail/mirrord/bijejadnnfgjkfdocgocklekjhnhkhkf) to route requests you make from Chrome to your local process. It works together with a running mirrord session that steals traffic with an [HTTP filter](../using-mirrord/incoming-traffic/filter-incoming-traffic.md): join the session from the extension popup (with `mirrord ui` running), and the extension injects the header matching the session's filter into every browser request. Hit a staging URL in Chrome and your local code answers, without changing any application code or configuring proxies. No operator? Set the header manually in the extension to match your session's filter. See [Debugging from Browser](../using-mirrord/incoming-traffic/debug-from-browser.md) for setup.

{% hint style="info" %}
**Working with a team?** [mirrord for Teams](https://app.metalbear.com) adds access control, traffic policies, and concurrent session management so your whole team can use mirrord safely.
{% endhint %}

## Configuration

mirrord reads config from `<project-path>/.mirrord/mirrord.json` (also supports `.toml` and `.yaml`). You can also prefix config files, e.g. `my-config.mirrord.json`.

Run `mirrord wizard` to generate a config file interactively, or see the full [configuration options](https://metalbear.com/mirrord/docs/config).

{% hint style="info" %}
The IDE extensions provide autocomplete for mirrord config files.
{% endhint %}

## Next Steps

**What are you trying to do?**

Not sure where to start? Run `mirrord wizard` to walk through common use cases interactively.

| Goal | Guide |
|------|-------|
| **Test against live traffic** | [Steal incoming traffic](../using-mirrord/incoming-traffic/filter-incoming-traffic.md) so your local process responds to real requests instead of the remote pod |
| **Debug several microservices together** | [`mirrord up`](../using-mirrord/multiple-concurrent-sessions.md) runs multiple sessions from a single config file and manages their lifecycle as one |
| **Test through your browser** | The [browser extension](../using-mirrord/incoming-traffic/debug-from-browser.md) routes requests from Chrome to your local process, no proxy setup needed |
| **Debug a queue consumer** | [Queue splitting](../sharing-the-cluster/queue-splitting.md) lets your local process consume messages without competing with the deployed service |
| **Run a tool in cluster context** | [Targetless mode](../using-mirrord/targetless.md) lets you run scripts or tools with cluster network access, without impersonating a specific pod |
| **Use mirrord with an AI agent** | [Set up Claude Code, Cursor, or Codex](../using-mirrord-with-ai/) to test generated code against your cluster automatically |
| **Set up for your team** | Install the [mirrord Operator](../managing-mirrord/operator.md) for access control, policies, and multi-user support |

Need help or want to share feedback? [Join our Slack community](https://metalbear.com/slack)
