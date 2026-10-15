---
title: Installing mirrord
description: How to install mirrord, and what your machine and cluster need
---

mirrord can be installed and used in several ways depending on your development workflow:

- **[CLI](cli.md)** - Install the mirrord command-line tool directly
- **[VS Code](vscode.md)** - Install the mirrord extension for Visual Studio Code and compatible editors (Cursor, Windsurf, etc.)
- **[JetBrains IDEs](intellij.md)** - Install the mirrord plugin for JetBrains IDEs (IntelliJ, PyCharm, GoLand, etc.)

For all the IDEs, coding agents, and other tools that mirrord works with, see [mirrord integrations](https://metalbear.com/mirrord/integrations/).

mirrord runs natively on Windows — no WSL needed. If you still prefer to use WSL, see our **[WSL setup guide](wsl.md)**.

## Local Requirements

For your local machine, you may use any of:
- MacOS (Intel, Apple Silicon)
- Linux (x86_64)
- Windows (x86_64), WSL (x86_64)

kubectl needs to be configured on the local machine.

## Cluster Requirements

mirrord supports Kubernetes `1.22` or later. This applies to the open-source version and to the [mirrord Operator](../managing-mirrord/operator.md). The nodes where agents run need Linux kernel `5.4` or later, and a containerd, CRI-O or Docker runtime.

Some features need a newer version of Kubernetes:

| Feature | Minimum Kubernetes version |
| --- | --- |
| Agent as an [ephemeral container](https://metalbear.com/mirrord/docs/config/options#agent-ephemeral) | `1.23` |
| AppArmor profile for the agent ([`agent.security_context`](https://metalbear.com/mirrord/docs/config/options#agent-security_context)) | `1.30` |
| [Queue splitting](../sharing-the-cluster/queue-splitting.md) | `1.30` |
| [Multi-cluster](../using-mirrord/multi-cluster-setup.md#prerequisites) | `1.30` on the Primary cluster |
| [Multi-cluster preview replicas](../using-mirrord/multi-cluster-setup.md#preview-environment-replicas) with database branching | `1.29` on the workload clusters |

## mirrord Operator

If you're planning to use [mirrord for Teams](https://app.metalbear.com), you'll also need to install the mirrord Operator in your cluster. See the [mirrord Operator](../managing-mirrord/operator.md) page for installation instructions.
