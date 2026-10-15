---
title: What is mirrord?
description: How mirrord works and what mirrord for Teams adds
---

# What is mirrord?

mirrord lets developers (or AI agents) run local code in the context of their cloud environment. Your code runs on your machine, but talks to real cloud services - databases, queues, APIs - as if it were deployed in the cluster.

When you start your process with mirrord, it overrides low-level system calls to:

* **Receive incoming traffic** from a remote pod (mirrored or stolen)
* **Send outgoing traffic** through the remote pod, reaching cluster-internal services
* **Read and write files** on the remote file system
* **Import environment variables** from the remote pod

mirrord runs in two places: a thin layer injected into your local process (`mirrord-layer`), and an agent pod in the cluster (`mirrord-agent`) that handles the remote side.

![mirrord - Basic Architecture](../.gitbook/assets/architecture.svg)

No VPN, no root access, no deployment. Your local process behaves as if it's running inside the cluster, but you keep your local tools, debugger, and IDE.

For the full architecture, see the [Architecture reference](../reference/architecture.md).

> **Try mirrord now** — it takes less than 2 minutes. Follow the [Quick Start guide](quick-start.md) to run your first local process in a cloud context.

{% hint style="info" %}
**Using an AI coding agent?** See [Using mirrord with AI agents](../using-mirrord-with-ai/) to set up autonomous testing against your cluster with Claude Code, Cursor, Codex, and more.
{% endhint %}

The same model powers **mirrord for CI** — running integration and end-to-end tests against a shared staging cluster without spinning up a dedicated test environment. See [mirrord for CI](../use-cases/mirrord-for-ci.md).

Working on several microservices at once? [`mirrord up`](../using-mirrord/multiple-concurrent-sessions.md) runs multiple mirrord sessions from a single config file, like `docker compose` for mirrord.

### mirrord for Teams

mirrord's core functionality is free and open source. Individual developers can install it and start using it immediately.

[mirrord for Teams](https://metalbear.com/mirrord/pricing) adds the **mirrord Operator** - a Kubernetes operator that runs in your cluster and acts as a centralized control plane. It enables:

* **Concurrent usage** - Multiple developers can work on the same cluster without conflicts
* **Traffic filtering** - Route only specific HTTP requests to your local process using header, path, or method filters
* **Queue splitting** - Split message queue traffic so each developer gets their own messages
* **DB branching** - Ephemeral database branches per developer session
* **Policies and profiles** - Admins can define rules and standardize mirrord configuration across the team
* **Session management** - View and manage all active mirrord sessions in the cluster

Features marked with **\[Teams]** in these docs require a mirrord for Teams license.

### mirrord for Enterprise

The Enterprise plan adds:

* **Preview Environments** - Deploy isolated, ephemeral pods in the cluster for async review and QA
* **mirrord for CI** - Run integration and end-to-end tests against a shared staging cluster without spinning up a dedicated test environment
* **Multi-cluster** - Intercept traffic across multiple clusters in a single mirrord session
* **Air-gapped operation** - Run the [License Server](../managing-mirrord/license-server.md) on-prem to manage seats without sending telemetry to MetalBear
* **High availability** - Run the Operator in HA mode
* **Premium support**

See [pricing](https://metalbear.com/mirrord/pricing) for details.
