---
title: High Availability
date: 2026-01-12T00:00:00.000Z
lastmod: 2026-06-07T00:00:00.000Z
draft: false
images: []
linktitle: High Availability
menu: null
docs: null
teams: null
weight: 505
toc: true
tags:
  - beta
  - enterprise
description: High availability for mirrord Operator in Enterprise Tier
---

{% hint style="info" %}
Session persistence — the ability to survive an Operator restart without terminating active mirrord sessions — is available to users on the Enterprise pricing plan.
{% endhint %}

{% hint style="warning" %}
The Operator's leader election and standby replica infrastructure is active regardless of license tier, so the Operator itself may recover from failures even on non-Enterprise plans. However, session persistence is not guaranteed outside of Enterprise. Sessions created on non-Enterprise plans are tied to a specific operator instance and are not guaranteed to survive a leader transition. We make no promises about HA behaviour for non-Enterprise plans.
{% endhint %}

Starting from chart version `1.40.1`, the mirrord Operator is highly available by default. This means mirrord sessions survive transient failures — including failures of the node where the Operator pod is running — without terminating the user's local process.

## How it works

HA is built on two mechanisms that work together:

**State persistence in etcd.** The Operator continuously saves session state as Kubernetes [Custom Resources](https://kubernetes.io/docs/concepts/extend-kubernetes/api-extension/custom-resources/), which are stored in the cluster's built-in etcd database. No external database or persistent volume is required. When an Operator pod starts (or restarts after a failure), it reads these Custom Resources and restores all in-progress sessions automatically.

**Leader election.** The Operator uses Kubernetes leader election to ensure that exactly one replica is active at any time. The elected leader serves all mirrord sessions. Any additional replicas remain in standby, holding the Kubernetes API watch loop open and ready to acquire leadership the moment the current leader becomes unavailable.

When a leader failure is detected, a standby replica acquires the leader lease, reads the persisted session state, and resumes active sessions. The time to recover depends on the Kubernetes leader-election timeouts and whether a warm standby replica is already running.

## Requirements

- **Enterprise license.** HA is an Enterprise-tier feature.
- **Helm chart `1.40.1` or later.** HA is enabled by default from this version; no additional configuration is required to enable it.
- **Standard Kubernetes cluster.** The Operator relies on the cluster's etcd (via the Kubernetes API) for state storage. No additional infrastructure is required.

## Configuration

### Replica count

By default the Operator runs with a single replica. A single-replica deployment is already highly available in the sense that sessions are persisted and automatically restored after a pod restart. However, with a single replica there is a cold-start delay during recovery: the new pod must be scheduled and started before leadership can be acquired.

Starting from chart version `1.40.3`, you can run multiple replicas for faster failover:

```yaml
# values.yaml
operator:
  replicas: 2
```

With multiple replicas, standby pods are already running when a leader failure occurs. The new leader is elected from the existing standby pool, which eliminates the pod scheduling delay and shortens the recovery window.

{% hint style="warning" %}
Additional replicas provide failover redundancy, not additional serving capacity. Only the elected leader handles mirrord sessions. A single Operator instance is a lightweight Kubernetes controller capable of managing hundreds of concurrent sessions across large clusters — additional replicas shorten recovery time after a failure, not throughput. If you are hitting session limits, see [Scalability](scalability.md).
{% endhint %}

### Recommended: pod anti-affinity

When running multiple replicas, schedule them on separate nodes so that a single node failure does not take down both the leader and all standbys at the same time:

```yaml
# values.yaml
operator:
  replicas: 2
  affinity:
    podAntiAffinity:
      preferredDuringSchedulingIgnoredDuringExecution:
        - weight: 100
          podAffinityTerm:
            labelSelector:
              matchLabels:
                app: mirrord-operator
            topologyKey: kubernetes.io/hostname
```

Using `preferredDuringSchedulingIgnoredDuringExecution` avoids blocking pod scheduling if only one node is available.

### Recommended: PodDisruptionBudget

To prevent Kubernetes from evicting all replicas simultaneously during voluntary disruptions (node drains, cluster upgrades), set a [PodDisruptionBudget](https://kubernetes.io/docs/tasks/run-application/configure-pdb/):

```yaml
apiVersion: policy/v1
kind: PodDisruptionBudget
metadata:
  name: mirrord-operator-pdb
  namespace: mirrord
spec:
  minAvailable: 1
  selector:
    matchLabels:
      app: mirrord-operator
```

## Caveats

### Not all advanced features are HA-ready

HA protects the core session lifecycle: the connection between the local process and its agent pod. However, some advanced mirrord for Teams features maintain additional state that has not yet been migrated to the persistent HA model. Sessions that use these features will be **forcefully terminated** if the Operator pod fails, rather than resumed.

The table below shows the current support status:

| Feature                     | HA  | Minimum chart version | Notes |
| --------------------------- | --- | --------------------- | ----- |
| SQS splitting               | ✅  | `1.40.3`              |       |
| RabbitMQ splitting          | ✅  | `3.152.0`             |       |
| GCP Pub/Sub splitting       | ✅  | `3.158.0`             |       |
| Azure Service Bus splitting | ✅  | `3.161.0`             |       |
| Kafka splitting             | ❌  |                       |       |
| Copy target                 | ❌  |                       | Opt-in HA available via `COPIED_PODS_HA=true` (see below) |
| DB branching                | ❌  |                       |       |

If a session is using a non-HA feature and the Operator pod fails, the user's local process will see the session end. They can restart mirrord to begin a new session once the Operator is back online.

#### Opt-in: Copy target HA

Copy target sessions can be made HA by setting the `COPIED_PODS_HA` environment variable to `true` on the Operator pod:

```yaml
# values.yaml
operator:
  extraEnv:
    - name: COPIED_PODS_HA
      value: "true"
```

When enabled, the Operator persists the copied pod name in the session status. After a leader transition, the new leader recovers the session by locating the existing copied pod rather than creating a new one. This is disabled by default because it requires the copied pod to still be running and reachable after the transition.

### Leadership transition window

Even with multiple warm standby replicas, there is a brief window during leader failover when no replica is serving sessions. The Operator uses a Kubernetes `Lease` with a 120-second duration, renewed every 60 seconds. A standby replica can acquire the lease as soon as the current holder stops renewing it — up to 120 seconds in the worst case. Sessions that were in-flight during this window may experience a short interruption before the new leader resumes them.

### etcd as a dependency

The Operator's HA model stores state in the Kubernetes API (backed by etcd). If the cluster's control plane itself is unavailable, the Operator cannot persist or restore session state. This is expected behavior for any Kubernetes workload that relies on the API server.
