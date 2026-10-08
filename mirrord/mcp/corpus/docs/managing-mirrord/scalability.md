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
