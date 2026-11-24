---
title: Onboarding wizard
date: 2026-01-05T12:00:00.000Z
lastmod: 2026-11-16T12:00:00.000Z
draft: false
toc: true
description: "Use the mirrord onboarding wizard to learn the basics and generate a mirrord-up.yaml."
tags:
  - oss
  - team
  - enterprise
---

# Onboarding Wizard

The onboarding wizard walks you through the basics of mirrord, then generates a `mirrord-up.yaml`: the file [`mirrord up`](../using-mirrord/multiple-concurrent-sessions.md) reads to run your local services against the cluster.

{% hint style="info" %}
The wizard generates a `mirrord-up.yaml` from mirrord 3.287.0.
{% endhint %}

## Launching the wizard

```bash
mirrord wizard
```

This opens the wizard in your browser. To answer the same questions in your terminal, run `mirrord wizard --tui`. See [`mirrord up init`](../using-mirrord/multiple-concurrent-sessions.md#mirrord-up-init) for details.

The wizard lists namespaces and workloads from the kube context mirrord uses: `kube_context` in your [global configuration](../installing-mirrord/global-configuration.md) if set, otherwise your kubeconfig's current context. Make sure you're connected to the cluster. Otherwise the wizard can't list targets or detect their ports.

From the landing page, you can go through a short overview of mirrord first, or go straight to the questions.

## Creating a configuration

For each service, the wizard asks for its mode, target, traffic, and local command. Then you can add another service or export the file. The file holds only what you set; anything left at its default is omitted.

### Mode

How the service shares incoming traffic with the deployed one: Split (the default), Mirror, or Replace. See [Service modes](../using-mirrord/multiple-concurrent-sessions.md#service-modes). Replace requires the operator.

### Target

Pick a namespace, then the workload the service runs against. In Split and Mirror, that's a deployment, statefulset, Argo rollout or pod; in Replace, a deployment or statefulset. In Split, you can instead run without a target, which gives the service outgoing traffic only.

Each service is an entry under `services` in `mirrord-up.yaml`, keyed by its name. The wizard names it after the workload, like `api` for `deployment/api`, adding `-2`, `-3`, and so on if that name is taken. A service without a target has no workload to name it after, so the wizard asks you for a name.

### Traffic

By default, Split and Mirror use the `baggage: .*mirrord-session={key}.*` header filter, and Replace takes all traffic, on every port your app listens on. You can change that by setting:

- an HTTP header filter (not in Replace), which applies to every intercepted port;
- which of the ports detected on the target to intercept;
- other ports to ignore.

The ports you don't intercept go to `ignore_ports`. Every other port your app listens on is intercepted.

### Local command

The command that starts the service, like `go run ./cmd/api`, and optionally its working directory, relative to the `mirrord-up.yaml`.

### Export

The wizard shows the services you added and a preview of the file, which you can copy or download. `mirrord wizard --tui` prints the preview and writes the file. Run it with:

```bash
mirrord up -f mirrord-up.yaml
```

For example, a Split service with a header filter and an ignored port, and a service without a target:

```yaml
services:
  server-echo:
    target:
      path: deployment/server-echo
    http_filter:
      header_filter: "^x-user: me$"
    ignore_ports: [9090]
    run:
      command: ["node", "server.js"]
  worker:
    target: none
    run:
      directory: jobs
      command: ["python", "worker.py"]
```

Settings shared by every service, like `operator` or `telemetry`, go under [`common`](../using-mirrord/multiple-concurrent-sessions.md#common), which the wizard doesn't ask for. See [Multiple concurrent sessions (mirrord up)](../using-mirrord/multiple-concurrent-sessions.md) for the full file format.
