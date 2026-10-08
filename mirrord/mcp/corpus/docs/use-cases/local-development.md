---
title: Local Development with Cloud Context
draft: false
images: []
toc: true
description: Run your code locally while it communicates with real cloud services
tags:
  - oss
  - team
  - enterprise
---

# Remocal Development

mirrord lets developers (or AI agents) run local code in the context of their cloud environment. Your code runs on your machine, but talks to real cloud services - databases, queues, APIs - as if it were deployed in the cluster.

We call this approach "remocal" (remote + local): local execution with remote context.

Want to see it in action? [Watch the demo](https://www.youtube.com/watch?v=ZR7A7cqQcFM).

### The problem

Traditionally, development happens in loops. You write and test code locally, then deploy to staging, where it meets production-like conditions for the first time. Tests fail. You fix, redeploy, repeat.

![The Traditional Dev Loop](../.gitbook/assets/devloop.png)

This is slow for two reasons:

1. **CI is a bottleneck** - Long test suites, flaky pipelines, and queued builds all add friction.
2. **Staging is fragile** - Deploying unstable code to a shared environment breaks it for everyone.

mirrord removes deployment from the loop entirely. Instead of deploying to test in the cloud, you plug your local process directly into the cloud environment.

### How it works

mirrord runs in two places - in the memory of your local process (`mirrord-layer`) and as a pod in the cluster (`mirrord-agent`).

![mirrord - Basic Architecture](../.gitbook/assets/architecture.svg)

When you start your process with mirrord, it overrides syscalls to:

* **Receive incoming traffic** from the remote pod (mirrored or stolen)
* **Send outgoing traffic** through the remote pod, reaching cluster-internal services
* **Read and write files** on the remote file system
* **Import environment variables** from the remote pod

The agent runs in the network namespace of the target pod and handles the remote side of these operations.

For the full architecture, see the [Architecture reference](../reference/architecture.md).

### What makes mirrord different

Other tools use VPNs to connect your machine to the cluster. mirrord works at the process level, overriding individual syscalls. This gives it unique advantages:

* **Fine-grained control** - Choose exactly what happens remotely vs locally. Read some files locally, others remotely. Route some traffic through the cluster, keep other connections local.
* **No root access needed** locally
* **Fast startup** - 15 seconds or less
* **Run multiple sessions** - Each local process can target a different remote pod simultaneously, and [`mirrord up`](../using-mirrord/multiple-concurrent-sessions.md) can start and manage several services from a single config file
* **Cluster-agnostic** - Works regardless of network setup (service mesh, VPN, etc.) and scales to 10,000+ pod clusters

[**Get started →**](../getting-started/quick-start.md)
