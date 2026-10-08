---
title: Migrating to MirrordSplitConfig
tags:
  - team
  - enterprise
---

All queue services now share a single configuration resource, `MirrordSplitConfig`. Earlier versions of mirrord used a different, broker-specific resource for each service. Those legacy resources are deprecated but still fully supported - the operator reads them on the fly and drives the split through the same unified flow, so existing setups keep working with no change.

This page shows how to move each legacy resource to `MirrordSplitConfig`. We recommend migrating so all your queue splitting configuration lives in one place. New setups should start with `MirrordSplitConfig` directly (see each broker's own page).

`MirrordSplitConfig` requires mirrord operator `3.170.0` or later. On earlier operators only the legacy resources are available.

{% hint style="info" %}
You do not have to migrate everything at once. The operator accepts a mix of legacy resources and `MirrordSplitConfig` objects, so you can migrate one workload at a time.
{% endhint %}

### Amazon SQS

Earlier versions of mirrord (`<3.170.0`) used the `MirrordWorkloadQueueRegistry` resource with `queueType: SQS`.

Here is the same configuration in the deprecated and the new format, side by side.

Deprecated `MirrordWorkloadQueueRegistry`:

```yaml
apiVersion: queues.mirrord.metalbear.co/v1alpha
kind: MirrordWorkloadQueueRegistry
metadata:
  name: meme-app-q-registry
  namespace: meme
spec:
  consumer:
    name: meme-app
    workloadType: Deployment
    container: main
  queues:
    meme-queue:
      queueType: SQS
      nameSource:
        envVar: INCOMING_MEME_QUEUE_NAME
      tags:
        tool: mirrord
      sns: true
    ad-queue:
      queueType: SQS
      nameSource:
        envVar: AD_QUEUE_NAME
```

Equivalent `MirrordSplitConfig` (plus a `MirrordPropertyList` for the per-queue options):

```yaml
apiVersion: queues.mirrord.metalbear.co/v1
kind: MirrordSplitConfig
metadata:
  name: meme-app-split
  namespace: meme
spec:
  targetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: meme-app
  queues:
    - id: meme-queue
      kind: sqs
      queueConfig: meme-queue-options
      appConfig:
        queue:
          - env: INCOMING_MEME_QUEUE_NAME
            containers:
              - main
    - id: ad-queue
      kind: sqs
      appConfig:
        queue:
          - env: AD_QUEUE_NAME
            containers:
              - main
---
apiVersion: mirrord.metalbear.co/v1
kind: MirrordPropertyList
metadata:
  name: meme-queue-options
  namespace: meme
spec:
  properties:
    - name: sns
      value: 'true'
    - name: tags
      value: '{"tool":"mirrord"}'
```

**Field mapping:**

| `MirrordWorkloadQueueRegistry` _(deprecated)_ | `MirrordSplitConfig` **(NEW)** |
| -------------------------------------------------- | ------------------------------ |
| `spec.consumer.name` / `workloadType` | `spec.targetRef.name` / `kind` (with `apiVersion`) |
| `spec.consumer.container` | `appConfig.queue[].containers` |
| `spec.queues.<key>` | `spec.queues[].id` |
| `queueType: SQS` | `kind: sqs` |
| `nameSource.envVar` | `appConfig.queue[].env` |
| `nameSource.regexPattern` | `appConfig.queue[].envLike` |
| `fallbackName` | `appConfig.queue[].fallback` |
| `namesFromJsonMap: true` | `appConfig.queue[].valueSelector: ".[]"` |

**Per-queue options.** In the old format `sns`, `s3Event`, and `tags` were set inline on each queue. Now they live in a `MirrordPropertyList`, and the queue links to it through `queueConfig`. A queue with none of these options (like `ad-queue` above) needs no `queueConfig` and no `MirrordPropertyList`.

| Old inline field | `MirrordPropertyList` property |
| ---------------- | ------------------------------ |
| `sns: true` | `sns: "true"` |
| `s3Event: true` | `s3_event: "true"` |
| `tags: {...}` | `tags` (JSON object string) |

**To migrate:**
1. Create the new `MirrordSplitConfig`. For any queue that had inline `sns`, `s3Event`, or `tags`, create a `MirrordPropertyList` with those options and point the queue's `queueConfig` at it.
2. Start a session and verify messages are split as expected.
3. Delete the old `MirrordWorkloadQueueRegistry`.

### Kafka

Earlier versions of mirrord (`<3.170.0`) used two resources to configure Kafka splitting: `MirrordKafkaTopicsConsumer` (which queues to split and how to find their names) and `MirrordKafkaClientConfig` (the Kafka client connection). The new `clientConfig` field transparently falls back to a `MirrordKafkaClientConfig` of the same name in the operator's namespace, so you can migrate the topics consumer first and the client config later.

Here is the same configuration in the deprecated and the new format, side by side.

Deprecated `MirrordKafkaTopicsConsumer` + `MirrordKafkaClientConfig`:

```yaml
apiVersion: queues.mirrord.metalbear.co/v1alpha
kind: MirrordKafkaTopicsConsumer
metadata:
  name: meme-app-topics-consumer
  namespace: meme
spec:
  consumerApiVersion: apps/v1
  consumerKind: Deployment
  consumerName: meme-app
  topics:
  - id: views-topic
    clientConfig: base-config
    groupIdSources:
    - directEnvVar:
        container: consumer
        variable: KAFKA_GROUP_ID
    nameSources:
    - directEnvVar:
        container: consumer
        variable: KAFKA_TOPIC_NAME
        fallback: views-topic
---
apiVersion: queues.mirrord.metalbear.co/v1alpha
kind: MirrordKafkaClientConfig
metadata:
  name: base-config
  namespace: mirrord # operator namespace
spec:
  properties:
  - name: bootstrap.servers
    value: kafka.default.svc.cluster.local:9092
```

Equivalent `MirrordSplitConfig` + `MirrordPropertyList`:

```yaml
apiVersion: queues.mirrord.metalbear.co/v1
kind: MirrordSplitConfig
metadata:
  name: meme-app-split
  namespace: meme
spec:
  targetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: meme-app
  queues:
    - id: views-topic
      kind: kafka
      clientConfig: kafka-connection
      appConfig:
        topic:
          - env: KAFKA_TOPIC_NAME
            fallback: views-topic
            containers:
              - consumer
        groupId:
          - env: KAFKA_GROUP_ID
            containers:
              - consumer
---
apiVersion: mirrord.metalbear.co/v1
kind: MirrordPropertyList
metadata:
  name: kafka-connection
  namespace: meme # same namespace as the target workload
spec:
  properties:
    - name: bootstrap.servers
      value: kafka.default.svc.cluster.local:9092
```

Field mapping:

| `MirrordKafkaTopicsConsumer` + `MirrordKafkaClientConfig` _(deprecated)_ | `MirrordSplitConfig` **(NEW)** |
| ---------------------------------------------------------------------------- | ------------------------------ |
| `consumerApiVersion` / `consumerKind` / `consumerName` | `spec.targetRef.apiVersion` / `kind` / `name` |
| `topics[].id` | `spec.queues[].id` |
| `topics[].nameSources[].directEnvVar` | `appConfig.topic[]` (`variable` -> `env`, `container` -> `containers`, `fallback` -> `fallback`) |
| `topics[].groupIdSources` | `appConfig.groupId[]` |
| `topics[].applicationIdSources` | `appConfig.appId[]` |
| `topics[].clientConfig` (a `MirrordKafkaClientConfig`) | `spec.queues[].clientConfig` (a `MirrordPropertyList` in the target namespace, or the same legacy name as a fallback) |
| `consumerRestartTimeout` | `spec.restart.timeout` |
| `splitTtl` | `spec.ttl` |

The `MirrordKafkaClientConfig` properties map one-to-one onto `MirrordPropertyList` properties. The only difference is the namespace: a `MirrordPropertyList` lives in the target's namespace, while `MirrordKafkaClientConfig` lives in the operator's namespace.

To migrate:
1. Create a `MirrordPropertyList` in the target's namespace with your Kafka client properties (or keep your existing `MirrordKafkaClientConfig` - the new `clientConfig` falls back to it by name).
2. Create the new `MirrordSplitConfig` using the mapping above.
3. Start a session and verify messages are split as expected.
4. Once you are confident, delete the old `MirrordKafkaTopicsConsumer` (and `MirrordKafkaClientConfig`, if you moved its properties into a `MirrordPropertyList`).

### RabbitMQ

Earlier versions of mirrord (`<3.170.0`) used the `MirrordWorkloadQueueRegistry` resource with `queueType: RMQ`.

Here is the same configuration in the deprecated and the new format, side by side.

Deprecated `MirrordWorkloadQueueRegistry`:

```yaml
apiVersion: queues.mirrord.metalbear.co/v1alpha
kind: MirrordWorkloadQueueRegistry
metadata:
  name: meme-app-q-registry
  namespace: meme
spec:
  consumer:
    name: meme-app
    workloadType: Deployment
    container: main
  queues:
    meme-queue:
      clusterProperties: meme-rmq-cluster
      queueType: RMQ
      nameSource:
        envVar: INCOMING_MEME_QUEUE_NAME
    ad-queue:
      clusterProperties: meme-rmq-cluster
      queueType: RMQ
      nameSource:
        envVar: AD_QUEUE_NAME
```

Equivalent `MirrordSplitConfig` (the `meme-rmq-cluster` `MirrordPropertyList` is unchanged):

```yaml
apiVersion: queues.mirrord.metalbear.co/v1
kind: MirrordSplitConfig
metadata:
  name: meme-app-split
  namespace: meme
spec:
  targetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: meme-app
  queues:
    - id: meme-queue
      kind: rmq
      clientConfig: meme-rmq-cluster
      appConfig:
        queue:
          - env: INCOMING_MEME_QUEUE_NAME
            containers:
              - main
    - id: ad-queue
      kind: rmq
      clientConfig: meme-rmq-cluster
      appConfig:
        queue:
          - env: AD_QUEUE_NAME
            containers:
              - main
```

Field mapping:

| `MirrordWorkloadQueueRegistry` _(deprecated)_ | `MirrordSplitConfig` **(NEW)** |
| -------------------------------------------------- | ------------------------------ |
| `spec.consumer.name` / `workloadType` | `spec.targetRef.name` / `kind` (with `apiVersion`) |
| `spec.consumer.container` | `appConfig.queue[].containers` |
| `spec.queues.<key>` | `spec.queues[].id` |
| `queueType: RMQ` | `kind: rmq` |
| `clusterProperties` | `clientConfig` |
| `queueProperties` | `queueConfig` |
| `nameSource.envVar` | `appConfig.queue[].env` |
| `nameSource.regexPattern` | `appConfig.queue[].envLike` |
| `fallbackName` | `appConfig.queue[].fallback` |
| `namesFromJsonMap: true` | `appConfig.queue[].valueSelector: ".[]"` |
| `exchangeSource` | `appConfig.exchange` |

`clusterProperties` and `queueProperties` already pointed to a `MirrordPropertyList` by name; `clientConfig` and `queueConfig` point to the same `MirrordPropertyList` the same way. These rows are just renames - there is no new resource to create.

**To migrate:**
1. Create the new `MirrordSplitConfig` using the mapping above. Your existing `MirrordPropertyList` cluster declaration is reused as-is.
2. Start a session and verify messages are split as expected.
3. Delete the old `MirrordWorkloadQueueRegistry`.
