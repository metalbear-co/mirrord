---
title: Queue Splitting Status
tags:
  - alpha
  - team
  - enterprise
---
# Queue Splitting Status

Once queue splitting is set up (see the per-service pages) you often want to know what is happening right now: which sessions are active, who started them, which queues the operator resolved, and whether the target pods were patched. mirrord exposes this in two ways that show the same data:

* The `mirrord queues` CLI command, for a quick human-readable view.
* A read-only Kubernetes-style API (`queuesplits`), for `kubectl` and scripts.

{% hint style="info" %}
Reading queue-splitting status requires mirrord operator `3.223.0` or later and mirrord CLI `3.223.0` or later.
{% endhint %}

{% hint style="info" %}
The status is a live view. It is synthesized on each request from the operator's in-memory state, so there is no stored object and nothing to clean up. When a session ends, its entry disappears.
{% endhint %}

## The `mirrord queues` command

`mirrord queues` (short alias `mirrord qs`) reads the status API and prints it. It uses the same Kubernetes access as any other mirrord command, so it works against whatever cluster your kubeconfig points at.

### Listing active sessions

Running `mirrord queues status` with no name lists one row per active split, with these columns:

* `NAME` - the split's resource name. Pass this to `mirrord qs status <name>` for the detailed view.
* `SESSION` - the mirrord session id, in the same uppercase hex format the operator status shows.
* `USER` - the developer who started the session, as `username/k8sUsername@hostname`.
* `NAMESPACE` - the namespace of the target workload.
* `TARGET` - the target workload as `Kind/name`.
* `PHASE` - `Pending`, `Ready`, or `Failed` (see [Phases](status.md#phases)).
* `DURATION` - how long the session has been running.

### Choosing a namespace

Like `kubectl`, the command lists a single namespace by default. It picks the namespace from, in order:

1. the `-n` / `--namespace` flag,
2. `target.namespace` from your mirrord config (`-f`),
3. the kubeconfig's default namespace.

Pass `-A` / `--all-namespaces` to list every namespace instead:

```shell
mirrord qs status -n events        # list splits in the "events" namespace
mirrord qs status -A               # list splits across all namespaces
```

### Inspecting one split

Pass a `NAME` from the list to see the full detail of a single split:

```shell
mirrord qs status e9f285ea68abf9f9.multi-broker-consumer.deployment
```

It prints a summary (session, namespace, user, target, phase, duration), then a table each for the filters requested in the mirrord config, the queues the operator resolved, and the target pods. A single session can split several queues, and those queues can be of different broker types - they all show up together here.

## The `queuesplits` API

The operator serves the same data under the `operator.metalbear.co/v1` API group as a `QueueSplit` resource (plural `queuesplits`). This is not a stored `CustomResourceDefinition` - the whole group is answered live from the operator, so `kubectl get`/`describe` work but you cannot create or edit these objects.

```shell
kubectl get queuesplits -A
kubectl get queuesplits -n events -o yaml
kubectl describe queuesplit e9f285ea68abf9f9.multi-broker-consumer.deployment -n events
```

An example object for a session splitting three brokers (SQS, Redis Pub/Sub, and Temporal) at once (each field is explained in the [field reference](status.md#field-reference) below):

```yaml
apiVersion: operator.metalbear.co/v1
kind: QueueSplit
metadata:
  name: e9f285ea68abf9f9.multi-broker-consumer.deployment
  namespace: events
spec:
  session: E9F285EA68ABF9F9
  target:
    apiVersion: apps/v1
    kind: Deployment
    name: multi-broker-consumer
    container: multi-broker-consumer
  owner:
    username: metalbear
    k8sUsername: metalbear
    hostname: metalbear-dev
    userId: bWV0YWxiZWFy
  filters:
    - id: multi-broker-sqs
      queueType: SQS
      messageFilter:
        client: ^a$
    - id: multi-broker-redis
      queueType: RedisPubSub
      messageFilter:
        tenant: ^test$
    - id: multi-broker-temporal
      queueType: Temporal
      messageFilter:
        workflow_id: ^test-
status:
  phase: Ready
  queues:
    - id: multi-broker-sqs
      type: SQS
      queue: E2ETest
    - id: multi-broker-redis
      type: RedisPubSub
      queue: e2e-mb-redis
    - id: multi-broker-temporal
      type: Temporal
      queue: order-checkout
  targetPods:
    - name: multi-broker-consumer-57b8665666-x65gh
      patched: true
      ready: true
```

### Field reference

`spec` mirrors what the user asked for:

* `session` - the mirrord session id.
* `target` - the split target workload (`apiVersion`, `kind`, `name`, `container`).
* `owner` - who started the session (`username`, `k8sUsername`, `hostname`, `userId`).
* `filters[]` - one entry per queue id in the user's mirrord config:
  * `id` - the queue id.
  * `queueType` - the broker type: `SQS`, `Kafka`, `RMQ`, `GooglePubSub`, `AzureServiceBus`, `RedisPubSub`, `Temporal`, `BullMQ`, `NATS`, or `NATSPubSub`.
  * `messageFilter` - attribute/header name to regex map (present only if set).
  * `jqFilter` - jq program applied to the message (present only if set).

`status` reflects what the operator resolved:

* `phase` - `Pending`, `Ready`, or `Failed` (see [Phases](status.md#phases)).
* `message` - a human-friendly detail, such as a failure reason (present only when there is one).
* `queues[]` - the queues the operator actually resolved from the target. There can be more than one per filter (for example when an `envLike` regex matches several environment variables). Each entry has an `id`, a `type`, and only the broker-specific names that apply: `queue` (SQS, RabbitMQ, Azure Service Bus, Temporal task queue), `topic` (Kafka, Azure Service Bus, Redis Pub/Sub channel, NATS stream, core NATS subject), `consumerGroup` (Kafka), or `subscription` (GCP Pub/Sub, Azure Service Bus, NATS durable consumer).
* `targetPods[]` - the target pods seen for the session, each with `patched` (carries the split's env-var patch) and `ready` (running with all containers ready).

### Phases

| Phase     | Meaning                                                                                     |
| --------- | ------------------------------------------------------------------------------------------- |
| `Pending` | The split is being set up: temporary queues are being created or target pods are restarting. |
| `Ready`   | All queues are resolved and every target pod is patched and ready. Messages are being split. |
| `Failed`  | Setup failed. See `status.message` (or the `MESSAGE` line in the detailed CLI view) for why. |

## Multi-cluster

In a multi-cluster setup the primary operator aggregates the view from every connected cluster, so `mirrord queues status -A` and `kubectl get queuesplits -A` against the primary show splits running in remote clusters too. A namespaced list (`-n <namespace>`, the default) also includes the splits the primary aggregates from other clusters for that namespace.
