---
title: Scalability
date: 2026-05-13T00:00:00.000Z
lastmod: 2026-05-13T00:00:00.000Z
draft: false
images: []
linktitle: Scalability
menu: null
docs: null
teams: null
weight: 507
toc: true
tags:
  - team
  - enterprise
description: Default sizing, concurrent session guidance, and capacity validation for the mirrord Operator
---

{% hint style="info" %}
This page is relevant for users on the Team and Enterprise pricing plans.
{% endhint %}

The mirrord Operator ships with default CPU and memory settings in MetalBear's public [Helm chart](https://github.com/metalbear-co/charts/blob/main/mirrord-operator/values.yaml). This page documents those defaults and provides guidance for validating capacity in your environment.

{% hint style="warning" %}
The defaults below are a starting point, not a performance SLA. Validated capacity should be established through testing in your own cluster with your own usage patterns.
{% endhint %}

## Default sizing

Unless overridden at install time, the operator uses the following resource defaults:

| Resource | Request | Limit  |
| -------- | ------- | ------ |
| CPU      | 100m    | 500m   |
| Memory   | 100Mi   | 200Mi  |

{% hint style="info" %}
The chart also defines separate resource defaults for optional components such as the Kafka splitting sidecar. If you enable advanced features, treat those as additional capacity planning items.
{% endhint %}

As described in [High Availability](high-availability.md), the default replica count is 1. Only one replica acts as the leader and serves sessions; additional replicas provide failover, not parallel session capacity.

## Footprint in your cluster

mirrord adds a small, mostly idle footprint to your cluster. In a default installation, the Operator is the only component that runs all the time. Optional components that you install separately, such as a license server, also run all the time. Agents run only while a session uses them.

### When no session is active

When no one uses mirrord, the only pod that runs is the Operator pod (one replica by default, with the sizing above). No agent pods run.

The installation also stores Kubernetes objects in the cluster, such as CRDs, RBAC objects, a Service, a ConfigMap, and a PriorityClass for agent pods. These objects are stored in etcd and do not use CPU or memory on your nodes.

### Agents

An agent is the component that does the work of a session inside the cluster. It mirrors or steals traffic, and it sends DNS, file, and outgoing network requests from the target's context. Unless overridden, each agent pod uses these resources:

| Resource | Request | Limit  |
| -------- | ------- | ------ |
| CPU      | 1m      | 1      |
| Memory   | 1Mi     | 100Mi  |

The requests are very small, so agent pods have almost no effect on scheduling. The limits stop an agent from using too much of a node.

The lifecycle of an agent depends on the type of session:

- **Session with a target:** The Operator makes one agent pod for each ready pod of the target. Sessions with the same target share these agent pods. When no session uses an agent pod, the Operator deletes it. For more details, see [Node Upgrades and Scale-Down](node-upgrades.md#agent-pods-and-nodes).
- **Targetless session:** All the targetless sessions in a namespace share one agent pod in that namespace. When no session uses the agent pod, the Operator deletes it.
- **Ephemeral agent:** The agent is a container in the target pod, not a separate pod. To use ephemeral agents, the cluster admin sets `ephemeral: true` under `agent.extraConfig` in the Operator Helm values. The Operator ignores the agent settings in the user's mirrord configuration. Kubernetes does not let you set resources on ephemeral containers, so the values above do not apply.

To change the agent resources, set `agent.resources` in the Operator Helm values (mirrord Operator `3.197.0` or later). A value that you set to `null` is left out of the agent pod spec. For example, set `agent.resources.limits.cpu: null` to run agents without a CPU limit.

{% hint style="info" %}
Some features allocate more resources for a session. [Copy target](../using-mirrord/copy-target.md) makes a copy of the target pod, [database branching](../sharing-the-cluster/db-branching.md) makes a branch database (for most engines, in a temporary pod), and [queue splitting](../sharing-the-cluster/queue-splitting.md) makes temporary queues. For more details, see the page for each feature.
{% endhint %}

## Concurrent sessions

The default resource envelope supports approximately 200 concurrent sessions. This is based on internal testing with the default CPU and memory limits, not a universal capacity guarantee.

- It is reasonable to use ~200 concurrent sessions as a starting-point expectation for the default resource envelope.
- Your actual ceiling depends on session churn (how frequently sessions are created and destroyed), which optional features are active, and your cluster's control-plane capacity.
- [Preview environments](../use-cases/preview-environments.md), [database branching](../sharing-the-cluster/db-branching.md), and [queue splitting](../sharing-the-cluster/queue-splitting.md) add control-plane and cluster work beyond the headline session count.

## When to tune defaults

Increase operator CPU and memory when [monitoring](monitoring.md) data shows the operator is consistently constrained (throttled CPU, memory approaching limits) while the cluster still has headroom and the API server is healthy.

Key [Prometheus metrics](monitoring.md#exposed-metrics) to watch:

| Metric | What it tells you |
| ------ | ----------------- |
| `mirrord_sessions_create_total` | Session creation rate and volume |
| `mirrord_sessions_duration` | Session lifetime distribution |
| `mirrord_operator_ping_latency` | Round-trip latency between clients and the operator — rising latency under load suggests resource pressure |

Combine these with standard Kubernetes signals: CPU and memory usage vs. requests/limits, pod restart counts, and node-level contention.

If you need to validate capacity for your specific environment, we recommend running a controlled ramp test in a non-production cluster with [monitoring](monitoring.md) enabled, gradually increasing concurrent sessions while observing the metrics above. [Contact us](https://metalbear.com/contact/) if you need guidance on capacity planning for your deployment.
