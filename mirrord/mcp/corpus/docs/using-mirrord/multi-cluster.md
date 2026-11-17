---
title: Seamless Multi-Cluster Development
description: >-
  Multi-cluster mirrord lets developers intercept traffic from pods across
  several Kubernetes clusters at once, without switching kube contexts.
tags:
  - alpha
  - enterprise
---

# Seamless Multi-Cluster Development

Multi-cluster mirrord lets developers intercept traffic from pods running in multiple Kubernetes clusters at the same time, without switching kube contexts or managing multiple sessions. You run `mirrord exec` exactly like before, and the operator handles everything behind the scenes.

**When is this useful?**

1.  **Multi-region traffic testing**

    Your service receives traffic from multiple regions such as `us-east-1` and `eu-west-1`. You want to validate your local changes against live traffic from all regions simultaneously, without running separate mirrord sessions per cluster.
2.  **Centralized access**

    Your organization requires developers to connect through a management cluster, with no direct access to workload clusters. You need mirrord to coordinate traffic interception across the workload clusters that share the same target, while operating from the approved entry point.
3.  **Shared environments with multiple clusters**

    Your staging environment spans multiple clusters. You want mirrord to intercept traffic across all of them in a single session.

***

## How It Works

In single-cluster mirrord, the CLI connects to one operator, which creates one agent that intercepts traffic. In multi-cluster, a **Primary** cluster coordinates sessions across multiple **Workload** clusters.

The developer connects to the Primary cluster. The Primary operator creates child sessions on each workload cluster. Each workload cluster's operator spawns an agent locally. Traffic from all clusters flows back to the developer's local application through a single connection.

![Multi-cluster architecture: the CLI connects to the Primary cluster, which coordinates child sessions on all workload clusters](../.gitbook/assets/multi-cluster-architecture.svg)

From the developer's perspective, nothing changes. The same `mirrord exec` command, the same configuration file, the same workflow. The operator decides whether to use single-cluster or multi-cluster mode based on its installation configuration.

***

## Understanding Cluster Roles

Each cluster in a multi-cluster setup takes on one or more roles. A cluster can have multiple roles at the same time (e.g. Primary + Workload + Default).

| Role         | What it does                                                                                       | How many    |
| ------------ | -------------------------------------------------------------------------------------------------- | ----------- |
| **Primary**  | Entry point for developers. Orchestrates sessions across all other clusters.                       | Exactly one |
| **Workload** | Runs application pods. Agents intercept traffic here.                                              | One or more |
| **Default**  | Handles stateful operations like (env vars, files, DNS, outgoing connections, DB branching etc..). | Exactly one |

### Primary Cluster

The Primary cluster is the developer's entry point. When you run `mirrord exec`, your CLI talks to the Primary cluster's operator. The Primary then orchestrates sessions across all other clusters.

The Primary cluster can also run workloads (it can be both a coordinator and a workload cluster), or it can be configured as management-only if it has no application pods.

### Workload Clusters

Workload clusters are where your application pods run. Each workload cluster has its own mirrord operator that spawns agents and handles traffic interception locally, just like single-cluster mirrord.

### Default Cluster

Some operations need to return consistent data such as environment variables, file reads, outgoing connections, and database branching. These are called "stateful operations." The Default cluster is the one cluster designated to handle all of them.

When your app reads `DATABASE_URL` for example, the request goes to the Default cluster only. You get one answer, not different answers from different clusters. Traffic operations (mirror/steal) still go to all workload clusters.

The default cluster is chosen in the settings of the operator on the primary cluster. Any workload cluster can be set to be the default cluster, including the primary cluster, if it's also a workload cluster.

### Management-Only Mode

If the Primary cluster only orchestrates and doesn't run application workloads, set it to management-only mode (`managementOnly: true`). In this case, you must designate a different cluster as the Default, since stateful operations can't run on a cluster with no application pods.

In management-only mode, the Primary resolves multi-cluster targets against the Default cluster, doesn't run [preview environment](../use-cases/preview-environments.md) controllers, and doesn't get the RBAC it needs to tunnel to branch databases locally.

Set `managementOnly` to `false` if the Primary runs application workloads, even when most sessions run on other clusters. With `managementOnly: false`, sessions started with `multi_cluster: false` run on the Primary as regular single-cluster sessions, using its local queue splitting and database branching controllers. With `managementOnly: true`, `multi_cluster: false` is not supported.

***

## How Operations Are Routed

During a session, the Primary operator routes each operation based on its type. Operations that need one consistent answer go to the Default cluster; operations that depend on where traffic lands go to every workload cluster.

| Operation                                        | Routed to                                                             |
| ------------------------------------------------ | --------------------------------------------------------------------- |
| Incoming traffic (mirror / steal)                | **All** workload clusters                                              |
| Environment variables                            | Default cluster only                                                   |
| File operations                                  | Default cluster only                                                   |
| DNS resolution                                   | Default cluster only                                                   |
| Outgoing traffic (TCP / UDP)                     | Default cluster only                                                   |
| [Database branching](#database-branching-in-multi-cluster) | Default cluster only (the branch is created once)           |
| [Queue splitting](#queue-splitting-in-multi-cluster) | **All** workload clusters \* (each filters messages locally)        |

\* One cluster only, when the split config names a [consumer cluster](#splitting-from-one-cluster).

Responses from all clusters are merged into the single connection back to your local application, so wherever a request or message lands, it reaches your local process the same way.

***

## Access Control

In multi-cluster mode, users only need access to the operator API on the **Primary cluster**. The Primary operator connects to downstream clusters using its own credentials, so users don't need any RBAC on downstream clusters. The per-target permission check does not apply in multi-cluster — if a user can reach the Primary operator, they can target workloads on any connected cluster.

To restrict what users can do per target, use [MirrordPolicy](../sharing-the-cluster/policies.md) resources on each cluster.

{% hint style="warning" %}
Access to the Primary operator API grants the ability to target workloads across all connected clusters. Use `MirrordPolicy` to limit operations per target.
{% endhint %}

***

## Session Lifecycle

When the developer runs `mirrord exec`, the CLI connects to the Primary cluster and creates a single parent session. The Primary operator then creates child sessions on each workload cluster. From the perspective of each Workload cluster, each child session behaves like a regular single-cluster session: the operator resolves the target, spawns an agent, and handles the traffic interception. The one difference is queue splitting with a [consumer cluster](#splitting-from-one-cluster) set: a cluster other than the consumer checks the session against its queue splitting policy and then runs it without a split.

Once all child sessions are ready, the CLI establishes a unified WebSocket connection to the Primary cluster. Messages are routed automatically based on the type of operation (see [How Operations Are Routed](#how-operations-are-routed)).

***

## Database Branching in Multi-Cluster

[Database branching](../sharing-the-cluster/db-branching.md) in multi-cluster mode behaves the same from the developer’s perspective, but may execute on a different cluster internally. The developer connects to the Primary cluster, but the branch is created on the Default cluster.

If the Primary cluster is not the Default cluster, a synchronization controller (`DbBranchSyncController`) runs on the Primary. It mirrors branch resources to the Default cluster and syncs the branch status back to the Primary. The CLI waits for the branch to be ready, then passes the branch name via connection parameters when creating the session, same flow as single-cluster.

If the Primary cluster is also the Default cluster, no synchronization is required. The branching controllers operate locally on the Primary, and the flow is identical to single-cluster behavior.

***

## Queue Splitting in Multi-Cluster

[Queue splitting](../sharing-the-cluster/queue-splitting.md) works in multi-cluster mode for **every supported queue service** — Amazon SQS, Kafka, RabbitMQ, Google Cloud Pub/Sub, Azure Service Bus, NATS (JetStream and core pub/sub), Redis Pub/Sub, Temporal, and BullMQ. A single session can even split several queue services at once, across all clusters. See the [Feature Support Matrix](../reference/feature-matrix.md) for maturity levels.

The flow is the same as single-cluster, applied in every cluster (or in [one cluster](#splitting-from-one-cluster), when the config names it):

1. **One config, synced everywhere.** You create your `MirrordSplitConfig` (and any `MirrordPropertyList` client configs) on the **Primary cluster** only. The operator automatically syncs them to all Workload clusters.
2. **One filter, applied everywhere.** When you start a session with [`feature.split_queues`](../sharing-the-cluster/queue-splitting.md#setting-a-filter), the Primary pushes your message filter to every child session, so every cluster applies the exact same filter.
3. **Messages are filtered where they land.** In each cluster, that cluster's operator splits the queue locally, just like single-cluster: the deployed workload is patched to read from a temporary queue, and the operator evaluates your filter on every incoming message.
4. **Matches go to your session, the rest flows on.** Messages that match your filter are routed to the temporary session queue that your local application (or preview environment) reads from. Everything that doesn't match keeps flowing to the deployed consumers, untouched.

![Multi-cluster queue splitting: each cluster's operator filters messages locally and routes matches to the session queue your local app reads from](../.gitbook/assets/multi-cluster-queue-splitting.svg)

The key property: **messages never travel cluster-to-cluster to reach you**. Wherever a message lands, that cluster's operator catches it and routes it to your session.

{% hint style="info" %}
Create your `MirrordSplitConfig` resources on the **Primary cluster**. The operator automatically syncs them to all Workload clusters — you don't need to create them on each cluster manually.
{% endhint %}

{% hint style="warning" %}
Each Workload cluster that splits needs its own credentials for the queue service (for example, AWS credentials with SQS permissions). This is independent of the multi-cluster authentication method — even if a cluster connects via bearer token, its operator still needs queue-service credentials to create temporary queues. With a [consumer cluster](#splitting-from-one-cluster) set, only that cluster needs the credentials, but the queue service's splitting still has to be switched on in every cluster's operator (`operator.sqsSplitting` and the other `*Splitting` chart values): the other clusters receive the session's queues for their policy check, and an operator with the feature off refuses the whole session.
{% endhint %}

To monitor active splits across all clusters, run `mirrord queues status -A` (or `kubectl get queuesplits -A`) against the Primary cluster — it [aggregates the view from every connected cluster](../sharing-the-cluster/queue-splitting/status.md#multi-cluster).

### Splitting From One Cluster

When only one cluster's copy of the workload consumes the queue, set `spec.consumerCluster` on the `MirrordSplitConfig` to that cluster's name. Only that cluster's operator then splits the queue for sessions going through the Primary (`mirrord exec`, `mirrord up`, `mirrord ci`, the IDE extensions): it runs the forwarder, creates the temporary queues and patches its copy of the workload. The other clusters' child sessions leave their copy of the workload and its queues untouched.

The consumer cluster can be any cluster the session spans, the [Default cluster](#default-cluster) or another one. Your local application still reads its environment variables and files from the Default cluster: when the consumer is another cluster, the Primary connects to it first, copies the split queue names its operator produced onto the Default cluster's child session, and the Default cluster's operator writes them into the environment variables and files it serves, as it does for a split of its own. Only the split queue names come from the consumer cluster. Every other environment variable and file (broker endpoint, region, credentials, reply queues) comes from the Default cluster's copy of the workload, and DNS and outgoing traffic stay on the Default cluster too. So the Default cluster's copy of the workload has to point at the same broker with the same settings as the consumer cluster's copy, and the queues the consumer cluster splits have to be reachable from the Default cluster's network. Each `MirrordSplitConfig` names its own consumer, so two workloads can consume their queues in two different clusters.

[`MirrordPolicy`](../sharing-the-cluster/policies.md) rules for queue splitting still apply on every cluster: each child session is checked against its own cluster's `splitQueues` policy with the filter you asked for, and only then does a cluster that is not the consumer run a plain session. Keep a policy on each cluster, as [above](#access-control).

{% hint style="warning" %}
`consumerCluster` does not apply to preview environments. Previews still split on every cluster. Multi-cluster copy target sessions span only the Default cluster, so they honor `consumerCluster` only when it names the Default cluster and are refused otherwise.
{% endhint %}

```yaml
apiVersion: queues.mirrord.metalbear.co/v1
kind: MirrordSplitConfig
metadata:
  name: orders-consumer
  namespace: staging
spec:
  targetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: orders-consumer
  consumerCluster: eu-west-1
  queues:
    - id: orders
      kind: sqs
      appConfig:
        queue:
          - env: ORDERS_QUEUE_NAME
```

The name is one of the keys of the Primary's `operator.multiCluster.clusters`, or the Primary's own `operator.multiCluster.clusterName` when it runs workloads. A session that splits queues is refused when the name matches none of the clusters it spans; the error lists them. A session that splits no queues never looks at the name.

If the workload's queues still come from a `MirrordWorkloadQueueRegistry` or a `MirrordKafkaTopicsConsumer`, use a `targetSelector` config that sets only `consumerCluster`. A config with `targetRef` replaces both, so the session would lose their queues.

Leave `consumerCluster` unset to keep splitting on every workload cluster. `consumerCluster` needs operator `3.233.0` or later on every cluster; the Primary refuses the session and names the cluster whose operator is older, or whose license does not include the feature.

### Per-Service Details

Most services behave identically everywhere, with small differences in where temporary resources are created. The list below describes a session without a [consumer cluster](#splitting-from-one-cluster). With one set, only that cluster creates temporary resources and patches its workload, for every service.

* **[Amazon SQS](../sharing-the-cluster/queue-splitting/sqs.md)** — each Workload cluster creates its own temporary queues and patches the target workload locally. Session queue names are shared between clusters so matched messages from every cluster reach your session.
* **[GCP Pub/Sub](../sharing-the-cluster/queue-splitting/gcp-pubsub.md)** — each Workload cluster creates its own fallback topic and subscription (what the deployed workload reads from). The per-session topic and subscription are created once on the Default cluster, and their names are shared with the other clusters for reuse.
* **[`mirrord subscribe`](subscribe.md#multi-cluster)** — one subscription against the Primary streams the events of every cluster, each naming the cluster that intercepted it.

***

## Preview Environments in Multi-Cluster

`operator.multiCluster.preview.mode` decides where a [preview environment](../use-cases/preview-environments.md)'s pods run:

| | `default-cluster` (the default) | `replicas` |
| --- | --- | --- |
| Preview pods run on | The Default cluster | Every workload cluster |
| HTTP reaches the preview from | The Default cluster only | Every cluster |
| Queue messages reach the preview from | Every cluster | Every cluster |

Everything else behaves the same in both modes. Previews do not read [`consumerCluster`](#splitting-from-one-cluster): they split on every cluster either way. With `replicas`, each cluster serves its own traffic and idles on it independently, and all replicas share one [branch database](../sharing-the-cluster/db-branching.md) - a write from any cluster is visible to all of them, and nothing reaches the source database.

[Code sync](../use-cases/preview-environments-code-sync.md) doesn't run on a multi-cluster operator, in either mode.

Replicas on other clusters reach the branch database through a tunnel between the operators, initiated by the Primary. Workload clusters need no route to the Default cluster, and no cluster credentials are ever placed in your namespaces.

![Preview replicas: every workload cluster serves its own traffic, and replicas reach the shared branch database through the operator tunnel](../.gitbook/assets/preview-h2.svg)

Use `replicas` when preview traffic enters through a load balancer spanning your clusters, or when previews should survive a cluster outage. Stay on `default-cluster` when traffic enters one cluster anyway (share links, steered requests) or when queue splitting is the main feature - its coverage is identical in both modes. Setup: [Preview Environment Replicas](multi-cluster-setup.md#preview-environment-replicas).

***

## Limitations

* [Targetless mode](targetless.md) is not supported in multi-cluster sessions — a target is required so the operator can resolve it on each Workload cluster.
* Multi-cluster sessions only start when connecting to the Primary cluster. Connecting directly to a downstream cluster starts a regular single-cluster session there.
* Every workload cluster must run the target. A multi-cluster session starts a child session on each workload cluster and fails if any of them cannot resolve the target, so clusters that run different services are not supported.
* `multi_cluster: false` is not supported with `managementOnly: true`. See [Management-Only Mode](#management-only-mode).
* The per-target RBAC check does not apply — see [Access Control](#access-control) and use [MirrordPolicy](../sharing-the-cluster/policies.md) to restrict operations per target.

***

## What's Next?

To set up multi-cluster, see the [Multi-Cluster Setup](multi-cluster-setup.md) guide, which covers authentication methods, Helm configuration, and RBAC.

{% content-ref url="multi-cluster-setup.md" %}
[multi-cluster-setup.md](multi-cluster-setup.md)
{% endcontent-ref %}

For which features are supported in multi-cluster mode and at what maturity level, see the [Feature Support Matrix](../reference/feature-matrix.md).
