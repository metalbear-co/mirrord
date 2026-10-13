---
title: "Queue Splitting"
tags:
  - team
  - enterprise
---
If your application consumes messages from a queue service, you should choose a configuration that matches your intention:

1. Running your application with mirrord without any special configuration will result in your local application competing with the deployed application (and potentially other mirrord runs by teammates) for queue messages.
2. Running your application with [`copy_target` + `scale_down`](../using-mirrord/copy-target.md#replacing-a-whole-deployment-using-scale_down) will result in the deployed application not consuming any messages, and your local application being the exclusive consumer of queue messages.
3. If you want to control which messages will be consumed by the deployed application, and which ones will reach your local application, set up queue splitting for the relevant target, and define a messages filter in the mirrord configuration. Messages that match the filter will reach your local application, and messages that do not, will reach either the deployed application, or another teammate's local application, if they match their filter.

{% hint style="info" %}
Queue splitting is currently available for [Amazon SQS](https://aws.amazon.com/sqs/), [Kafka](https://kafka.apache.org/), [RabbitMQ](https://www.rabbitmq.com), [Google Cloud Pub/Sub](https://cloud.google.com/pubsub), [Azure Service Bus](https://azure.microsoft.com/en-us/products/service-bus), [NATS](https://nats.io) (JetStream and core pub/sub), [Redis Pub/Sub](https://redis.io/docs/latest/develop/interact/pubsub/), [Temporal](https://temporal.io), and [BullMQ](https://bullmq.io/).
The word "queue" in this doc is used to also refer to "topic" in the context of Kafka and Azure Service Bus, "subscription" in the context of Google Cloud Pub/Sub, "stream" or "subject" in the context of NATS, "channel" in the context of Redis Pub/Sub, and "task queue" in the context of Temporal.
{% endhint %}

{% hint style="info" %}
Queue splitting also works when your environment spans several Kubernetes clusters — see [Queue Splitting in Multi-Cluster](../using-mirrord/multi-cluster.md#queue-splitting-in-multi-cluster).
{% endhint %}

### Choose your queue service

Setup and configuration differ per queue service. Pick the one you use to see the full guide:

* [Amazon SQS](queue-splitting/sqs.md)
* [Kafka](queue-splitting/kafka.md)
* [RabbitMQ](queue-splitting/rabbitmq.md)
* [Google Cloud Pub/Sub](queue-splitting/gcp-pubsub.md)
* [Azure Service Bus](queue-splitting/azure-service-bus.md)
* [NATS](queue-splitting/nats.md)
* [Redis Pub/Sub](queue-splitting/redis-pubsub.md)
* [Temporal](queue-splitting/temporal.md)
* [BullMQ](queue-splitting/bullmq.md)

## How It Works

When a queue splitting session starts, the mirrord operator patches the target workload (e.g. deployment or rollout) to consume messages from a different, temporary queue.
That temporary queue is *exclusive* to the target workload.
Similarly, the local application is reconfigured to consume messages from its own *exclusive* temporary queue.

{% hint style="warning" %}
Queue splitting requires that the application read the queue name from an environment variable, from a config file mounted from a ConfigMap volume (see [Queue Names in Mounted Config Files](#queue-names-in-mounted-config-files)), or from a file injected into its pods, for example by Vault (see [Queue Names Injected by Vault or CSI Drivers](#queue-names-injected-by-vault-or-csi-drivers)).
This lets the operator override the name to change the queue that the application reads from.
{% endhint %}

Once all temporary queues are prepared, the mirrord operator starts consuming messages from the original queue, and publishing them to one of the temporary queues, based on message filters provided by the users in their mirrord configs.

Each message routed to a mirrord session also produces a [`Message Processing` functional log](../managing-mirrord/monitoring.md#message-processing) containing the session key, routing mode, queue or topic name, and any correlation or tracing metadata provided by the broker. These logs make routed messages queryable through the Operator's existing log collection pipeline without requiring a `mirrord subscribe` process.

Each queue service has its own way of creating temporary queues and routing messages. The per-service pages above walk through the exact behavior, including the diagrams for the first and second concurrent sessions.

Temporary queues are managed by the mirrord operator and garbage collected in the background. After all queue splitting sessions end, the operator promptly deletes the allocated resources.

Please note that:
1. Temporary queues created for the deployed targets will not be deleted as long as there are any targets' pods that use them.
2. In case of SQS splitting, deployed targets will keep reading from the temporary queues as long as their temporary queues have unconsumed messages.
3. For Google Cloud Pub/Sub, the operator creates temporary topics and subscriptions. The target workload's subscription environment variable is patched to read from a temporary subscription, while the operator drains the original subscription and forwards messages through temporary topics.

## Queue Names in Mounted Config Files

{% hint style="info" %}
Mounted config file sources require mirrord operator `3.198.0` or later.
{% endhint %}

A queue name can also be read from a file mounted from a ConfigMap volume, instead of an environment variable. This is useful when your application keeps its queue names in a config file - for example a Spring-style `application.yaml` in a centrally managed ConfigMap - and duplicating the names into the pod's environment is not an option.

To use it, set a `volume` source in the `appConfig` entry instead of `env` or `envLike`. A complete `MirrordSplitConfig` for a Kafka consumer reading both its topic and group from a mounted `application.yaml`:

```yaml
apiVersion: queues.mirrord.metalbear.co/v1
kind: MirrordSplitConfig
metadata:
  name: my-split-config
  namespace: my-namespace
spec:
  targetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: my-consumer
  queues:
    - id: orders                    # the queue ID users reference in split_queues
      kind: kafka
      clientConfig: kafka-connection
      appConfig:
        topic:
          - volume:
              name: app-config          # entry in the pod spec's `volumes` array
              file: application.yaml    # file within the volume
            valueSelector: ".kafka.consumer.topic.main.name"
        groupId:
          - volume:
              name: app-config
              file: application.yaml
            valueSelector: ".kafka.consumer.group"
```

The `volume` source fields:

* `volume.name` - name of a `configMap` volume in the target's pod spec. Other volume types are rejected.
* `volume.file` - path of the file within the volume: the ConfigMap data key, or the item `path` when the volume remaps keys via `items`.
* `valueSelector` - a selector run over the parsed file (JSON or YAML). It supports nested keys (`.kafka.consumer.group`) and `.[]` to iterate arrays or object values (`.topics.[]`); pipes, functions, and other jq operators are not supported.
* `valuePattern` - a regex whose capture group marks the name inside the raw file text. With neither `valueSelector` nor `valuePattern`, the whole file content is the queue name.

If both a `volume` source and an `env`/`envLike` source are set on the same entry, `env`/`envLike` takes precedence and `volume` is ignored. `fallback` does not apply to `volume`. A `containers` list is not needed either - the file is shared by every container that mounts the volume.

The operator never modifies your ConfigMap. When a split starts, it:

1. Creates a copy of the ConfigMap with the temporary fallback names substituted. The copy is labeled and managed by the operator.
2. Redirects the volume in the target's pods to the copy. This restarts the workload, the same way environment variable injection does.
3. Serves your local application a version of the file carrying its own session queue names, in-flight over the mirrord session. Nothing containing session names is written to the cluster.

When the last session ends, the pods are restored to the original ConfigMap and the copy is deleted.

Things to know:

* `valueSelector` rewrites re-serialize the copied file, so YAML comments and formatting are lost in the copy (never in your original). `valuePattern` rewrites keep the file byte-identical outside the swapped name.
* Your local application must read the mounted path through mirrord's remote file system. If your mirrord config marks that path as local (`feature.fs` local patterns), the local app reads its own file and never sees the session queue names.
* The session file content is served for reads that open the file by its full path. Reads that go through a directory file descriptor with a relative path (`openat` after opening the directory) bypass the override, and the local application then sees the copy's fallback names instead of its session names. Most applications open config files by full path and are unaffected.
* Editing the original ConfigMap while a split is running does not update the copy. Content changes are picked up when the next split starts.

### Preview Environments

A [preview environment](../use-cases/preview-environments.md) pod runs in the cluster and reads real mounted files, so the operator delivers session queue names by preparing the files each reader mounts. The queue names are located inside each file with the same `valueSelector` / `valuePattern` from the split config (see [the field reference above](#queue-names-in-mounted-config-files)). With a split running, every reader of the "same" config file sees its own version - and your original ConfigMap is never modified:

![Preview environments with mounted-config queue splitting](../.gitbook/assets/preview-configmap-split.svg)

Which queues get split, and where their names live, comes from the same two places as any split session - nothing preview-specific to set up:

* The cluster's `MirrordSplitConfig` for the target defines the queue IDs and points at the file with `volume` sources (the [`appConfig` setup above](#queue-names-in-mounted-config-files)).
* Your mirrord config picks the queue IDs to split and the message filter with [`feature.split_queues`](#setting-a-filter-for-a-mirrord-run), next to the `config_mounts` entry:

```json
{
  "feature": {
    "split_queues": {
      "orders": {
        "queue_type": "Kafka",
        "message_filter": { "user_id": "^my-preview$" }
      }
    },
    "preview": {
      "config_mounts": [
        {
          "mount_at": "/config-pr/application.yaml",
          "from_file": "./application.yaml"
        }
      ]
    }
  }
}
```

The per-session copies are owned by the `PreviewSession` and deleted with it; the fallback copy goes when the last session ends.

`config_mounts` entries compose with this. When the file content your mirrord config sends (`feature.preview.config_mounts` in `mirrord.json`) carries the same queue config, the operator bakes the session's queue names into it before mounting, and keeps every other value. For a session whose split renamed `orders`:

Content your `mirrord.json` provided:

```yaml
kafka:
  consumer:
    topic: orders
logLevel: debug       # your own override
```

Content the preview pod mounts:

```yaml
kafka:
  consumer:
    topic: mirrord-tmp-a1b2c3-orders   # this session's queue
logLevel: debug                        # kept as-is
```

The rewrite is content-based, not path-based: a mount can sit anywhere, and a mount whose content does not carry the split's names is left byte-identical.

## Queue Names Injected by Vault or CSI Drivers

{% hint style="info" %}
Injected file sources require mirrord operator `3.201.0` or later.
{% endhint %}

A queue name can also be read from a file that exists only inside the running pods, with no ConfigMap or Secret behind it. This is useful when vault-agent-injector renders the names into `/vault/secrets/`, or a secrets-store CSI driver projects them at mount time, and moving them into the pod's environment is not an option.

To use it, set a `podFile` source in the `appConfig` entry:

```yaml
appConfig:
  topic:
    - podFile:
        path: /vault/secrets/kafka-config   # absolute path inside the container
      valueSelector: ".kafka.consumer.topic.main.name"
```

* `podFile.path` - absolute path of the file inside the container.
* `podFile.container` - container the operator reads the file from. Defaults to the `vault-agent` sidecar when the pod has one, otherwise the pod's first application container. Set it when the file is only mounted in a specific container, or when the default container has no `cat` binary (a distroless image).
* `valueSelector` and `valuePattern` work exactly as for `volume` sources above.

Because no API object holds the file, the operator reads it by running `cat` in a running pod of the target. The target must have at least one running pod when the split starts, and the operator needs `get` and `create` on `pods/exec` in the target namespace - the operator Helm chart grants this when queue splitting is enabled.

The operator never touches Vault or the injector. When a split starts, it:

1. Creates a Secret with the file's content and the temporary fallback names substituted. The Secret is labeled and managed by the operator, holds only the referenced file, and also caches the original content so later resolutions never depend on the pods again.
2. Mounts that Secret over the file's exact path in the target's application containers. This restarts the workload, the same way environment variable injection does. The injector's own sidecar keeps its original view of the file, so it can keep rendering it underneath.
3. Serves your local application a version of the file carrying its own session queue names, in-flight over the mirrord session, exactly as for `volume` sources.

When the last session ends, the pods are restored to the injected file and the Secret is deleted.

Things to know:

* The referenced file's content is pinned for the length of the split. If the same file also carries values that rotate, such as credentials, the deployed application keeps reading the values from when the split started. Other injected files are untouched.
* If both a `podFile` source and an `env`/`envLike` or `volume` source are set on the same entry, the other source takes precedence and `podFile` is ignored. `fallback` does not apply to `podFile`.
* A `containers` list on the entry limits which containers get the overriding mount. Without one, every application container gets it.
* The remote file system and directory file descriptor notes for `volume` sources apply here as well.

## Autoscaled Targets with KEDA

{% hint style="info" %}
Scaling an autoscaled target on its temporary queue requires mirrord operator `3.215.0` or later, and operator Helm chart `3.215.0` with the `operator.manageKedaScaledObjects` value set to `true`.
{% endhint %}

A split target reads a temporary queue, while the triggers of its KEDA `ScaledObject` still read the original queue, which the operator drains. KEDA then sees no load and scales the target to zero, leaving the temporary queue undrained.

Set `operator.manageKedaScaledObjects` in the operator's Helm values to have the operator handle this. While a split is running, the operator points every trigger on the target's `ScaledObject` that reads a split queue at the temporary queue the target now consumes, and points them back at the original queue when the split ends. KEDA keeps scaling the target on the depth of the queue it is actually reading, so while that queue is empty, it scales the target in as far as its `minReplicaCount` and other triggers allow.

Only the queue a trigger reads is changed. Its credentials and every other setting stay as they are, and triggers about anything else, such as CPU, are left alone.

Triggers are redirected wherever a split moves the target onto a temporary queue: Amazon SQS, Apache Kafka, Google Cloud Pub/Sub, Azure Service Bus queues, BullMQ, NATS JetStream, and RabbitMQ. Everywhere else, including Azure Service Bus subscriptions, the target keeps reading its own queue, and its triggers need no change.

A Temporal target polls a task queue the operator serves, which KEDA cannot measure, so KEDA is paused at one replica for the duration of the split instead.

A `ScaledObject` none of whose triggers name a split queue directly is left alone. This includes triggers that read the queue name from an environment variable, such as `queueURLFromEnv`, and triggers that measure lag through a query. Unless they measure the temporary queue some other way, the target may be scaled to zero for the duration of the split. A Kafka target keeps its consumer group unless the split gives it a temporary one, so a query over that group's lag across all topics already follows the temporary topic.

A GitOps tool deploying the `ScaledObject` would put the original triggers back. The operator therefore registers a mutating webhook that reapplies its rewrite to every update of a redirected `ScaledObject`, including the dry runs Flux compares against. Flux sees the `ScaledObject` as in sync and keeps reconciling it: changes from Git roll out during the split, and the triggers stay redirected. When the split ends, the operator undoes only its own rewrite, so those changes stay.

Argo CD compares against Git by itself, so it would report the `ScaledObject` as out of sync and keep syncing it. Set `operator.applicationPauseAutoSync` to let the operator turn off automated sync on the application deploying the `ScaledObject`, and on every application deploying that one, for as long as the split runs. Changes to anything they deploy are not rolled out until the split ends. Without the value, a `ScaledObject` that Argo CD deploys is left alone.

With `operator.applicationPauseAutoSync` set, the operator finds the applications deploying the `ScaledObject` from their own status, whichever way Argo CD tracks resources. Without it, the operator recognizes a `ScaledObject` that Argo CD deploys only by its `argocd.argoproj.io/tracking-id` annotation, so if Argo CD tracks resources by label, the triggers are redirected anyway, and Argo CD reports the application out of sync and keeps syncing it until the split ends.

## Sharing Property Lists Across Namespaces

{% hint style="info" %}
Looking up a property list in the operator's namespace requires mirrord operator `3.191.0` or later. Earlier operators only look in the target's namespace.
{% endhint %}

Every queue service is set up with a `MirrordPropertyList` holding the broker connection details, referenced by name from the `MirrordSplitConfig`. The operator looks that name up in two places, in order:

1. the namespace of the target workload, which is also the namespace of the `MirrordSplitConfig`,
2. the namespace the operator is installed in.

This is useful when one broker serves many teams. Define the credentials once next to the operator, and every `MirrordSplitConfig` in the cluster can reference that name without each namespace keeping its own copy. A list in the target's namespace still wins, so a team can override the shared one by creating a list with the same name next to their workload.

ConfigMap and Secret references inside a property list are resolved in the namespace the list was found in. A list in the operator's namespace must therefore reference ConfigMaps and Secrets in the operator's namespace.

{% hint style="warning" %}
A property list in the target's namespace that the operator cannot parse fails the session instead of falling through to the operator's namespace. This keeps a broken local list from silently switching the target onto shared credentials.
{% endhint %}

## Session Key Header

When your operator has session key header injection enabled, every message the operator routes to your session is stamped with a `mirrord-key` carrying your session key, so your local application can tell which mirrord session a message belongs to. Only the copy delivered to your session is stamped; the message the deployed application receives is never modified.

Where the key is placed depends on the queue service. Services with a metadata channel carry the key there; the rest carry it inside the JSON payload.

| Queue service | Carrier | Location of `mirrord-key` |
| --- | --- | --- |
| Amazon SQS | Metadata | Message attribute |
| Google Cloud Pub/Sub | Metadata | Message attribute |
| RabbitMQ | Metadata | Message header |
| Apache Kafka | Metadata | Message header |
| Azure Service Bus | Metadata | Application property |
| Temporal | Metadata | Activity task header |
| NATS | Metadata | Message header |
| NATS Pub/Sub | Metadata | Message header |
| BullMQ | JSON payload | Job `data` object |
| Redis Pub/Sub | JSON payload | Message payload |

For BullMQ and Redis Pub/Sub, where the key lives in the JSON payload itself: a payload that is not a JSON object is forwarded unchanged, and an existing `mirrord-key` in the message is never overwritten.

For Temporal, only the activity task is stamped. Workflow tasks are not, because their header lives in workflow history that the worker replays and validates against the server.

{% hint style="info" %}
Injection is enabled through the `operator.injectSessionKeyHeader` setting in the [mirrord-operator Helm chart](https://github.com/metalbear-co/charts/blob/main/mirrord-operator/values.yaml).
{% endhint %}

{% hint style="info" %}
The key is not injected in copy-target mode. A copy-target session runs against its own dedicated copy of the workload, so no shared consumer needs to tell sessions apart, and messages routed to the copy are delivered unchanged.
{% endhint %}

## Setting a Filter for a mirrord Run

Once cluster setup is done, mirrord users can start running sessions with queue message filters in their mirrord configuration files.
[`feature.split_queues`](https://metalbear.com/mirrord/docs/config/options#feature-split_queues) is the configuration field they need to specify in order to filter queue messages.
It pairs each queue ID with a queue filter definition, and accepts either an object keyed by queue ID or an array of entries (see [One queue or many](#one-queue-or-many)).

Filter definition contains the following fields:
* `queue_type` - `SQS`, `Kafka`, `RMQ`, `GCPPubSub`, `AzureServiceBus`, `RedisPubSub`, `Temporal`, `BullMQ`, `NATS`, or `NATSPubSub`
* `queue_mode` - optional, `steal` (default) or `mirror`. In `steal` mode, a matched message goes only to your local application. In `mirror` mode, a matched message goes to your local application **and** is still delivered to the deployed application, so both process a copy. Not supported for `Temporal`.
* `filter` - a composable message filter, shaped like the [HTTP filter](../using-mirrord/incoming-traffic/filter-incoming-traffic.md): one `metadata` regex, or an `any_of` / `all_of` list of `metadata` regexes.
  A `metadata` regex is matched against every message attribute (SQS, GCP Pub/Sub), header (Kafka, RabbitMQ, NATS, NATS pub/sub), application property (Azure Service Bus), JSON field (Redis Pub/Sub, BullMQ), or task metadata entry (Temporal) rendered as `<name>: <value>`, the same way the HTTP filter sees headers.
  The message matches when any attribute line matches, so one regex can pin an attribute by name (`^tenant: blue$`) or find a marker wherever it is propagated (`.*mirrord-session={{ key }}.*`). Matching is case sensitive. See [Composing filters](#composing-filters).
* `message_filter` - the older shape: a mapping from an attribute name to a regex for its value.
  The local application will only see queue messages that have **all** of the specified entries matching. Still supported; use either `filter` or `message_filter` on an entry, not both.
* `jq_filter` - supported for `SQS`, `Kafka`, `RMQ`, `GCPPubSub`, `AzureServiceBus`, `RedisPubSub`, `Temporal`, `BullMQ`, `NATS`, and `NATSPubSub` queue types.
  * For **SQS**, it runs a jq program on the JSON representation of the SQS [`Message`](https://docs.aws.amazon.com/AWSSimpleQueueService/latest/APIReference/API_Message.html) object.
    For queues configured with `s3_event: "true"`, jq filters can also inspect `S3Metadata`.
    It is populated with user-defined S3 object metadata when the message is parsed as an S3 event
    and metadata is fetched successfully. `S3Metadata` follows the [AWS S3 user-defined metadata format](https://docs.aws.amazon.com/AmazonS3/latest/userguide/UsingMetadata.html#UserMetadata):
    a flat key-value map where keys are lowercase strings (without the `x-amz-meta-` prefix) and values are strings.
  * For **Kafka**, it runs a jq program on a JSON representation of the record. See the [Kafka page](queue-splitting/kafka.md#setting-a-filter) for the document shape.
  * For **RabbitMQ**, it runs a jq program on a JSON representation of the message. See the [RabbitMQ page](queue-splitting/rabbitmq.md#setting-a-filter) for the document shape.
  * For **GCP Pub/Sub**, it runs a jq program on the JSON representation of the [`PubsubMessage`](https://cloud.google.com/pubsub/docs/reference/rest/v1/PubsubMessage) object.
    For subscriptions configured with `gcs_event: "true"`, jq filters can also inspect `gcsMetadata`, the custom metadata of the Cloud Storage object a notification is about.
    See [Filtering Cloud Storage notifications](queue-splitting/gcp-pubsub.md#filtering-cloud-storage-notifications).
  * For **Azure Service Bus**, the JSON object has `body`, `application_properties`, `message_id`, `content_type`, and `subject` fields.
  * For **Redis Pub/Sub**, it runs a jq program on the parsed JSON message payload.
  * For **Temporal**, it runs a jq program on a JSON document the operator builds for each task. See the [Temporal page](queue-splitting/temporal.md#setting-a-filter) for the document shape.
  * For **BullMQ**, it runs a jq program on the parsed JSON value of the job's `data` field.
  * For **NATS** and **NATS pub/sub**, the JSON object has `subject`, `headers`, and `payload` fields. `payload` is the message body parsed as JSON when the body is JSON, and a string otherwise.
  * A message matches if the jq program outputs `true`.
* `payload_protobuf` - optional, `Kafka` only. Decodes record values that carry plain protobuf instead of JSON with a schema you provide, and exposes the decoded message to `jq_filter` as a `payload_decoded` field. See [Filtering on protobuf payloads](queue-splitting/kafka.md#filtering-on-protobuf-payloads).

If a `filter` (or `message_filter`) and a `jq_filter` are specified for the same queue, both must match for a message to be matched.

#### Composing filters

`filter` takes one of three forms. A single `metadata` regex:

```json
{
  "feature": {
    "split_queues": {
      "orders": {
        "queue_type": "SQS",
        "filter": { "metadata": "^tenant: blue-.*$" }
      }
    }
  }
}
```

`any_of` matches when at least one of the listed `metadata` regexes matches, `all_of` when every one does:

```json
{
  "feature": {
    "split_queues": [
      {
        "queue_id": "*",
        "queue_type": "Temporal",
        "filter": {
          "any_of": [
            { "metadata": "^header.baggage: .*mirrord-session={{ key }}.*$" },
            { "metadata": "^header.test: .*mirrord-session={{ key }}.*$" }
          ]
        }
      },
      {
        "queue_id": "orders",
        "queue_type": "Kafka",
        "filter": {
          "all_of": [
            { "metadata": "^tenant: blue$" },
            { "metadata": "^region: eu-.*$" }
          ]
        }
      }
    ]
  }
}
```

A `message_filter` of `{ "tenant": "^blue$", "region": "eu" }` is the same as `filter: { "all_of": [ { "metadata": "^tenant: blue$" }, { "metadata": "^region: .*eu" } ] }`, except that `message_filter` requires the attribute name to match exactly while a `metadata` regex sees the whole `name: value` line.

{% hint style="warning" %}
`filter` requires mirrord `3.264.0` or later, and mirrord operator `3.212.0` or later. Against an older operator the CLI refuses to start the session and names the missing feature; `message_filter` keeps working there.
{% endhint %}

Queue filter policies (`splitQueues` in a mirrord policy) check `message_filter` entries and `all_of` / `any_of` branches by attribute name. A `metadata` regex cannot prove which attribute it filters on, so on a queue covered by such a policy rule it is rejected the same way a lone `jq_filter` is.

#### One queue or many

`feature.split_queues` accepts two shapes.

For a single queue, use the **object** form, which maps the queue ID to its queue split config:

```json
{
  "feature": {
    "split_queues": {
      "orders": {
        "queue_type": "SQS",
        "message_filter": { "region": "^eu" }
      }
    }
  }
}
```

For multiple queues, use the **array** form, which moves the ID into each entry as `queue_id`:

```json
{
  "feature": {
    "split_queues": [
      {
        "queue_id": "orders",
        "queue_type": "SQS",
        "message_filter": { "region": "^eu" }
      },
      {
        "queue_id": "notifications",
        "queue_type": "RedisPubSub",
        "message_filter": { "tenant": "^test$" }
      }
    ]
  }
}
```

Both forms take the same filter fields (`queue_type`, `filter`, `message_filter`, `jq_filter`, `payload_protobuf`). Unlike the object form, the array form also lets the **same** queue ID be split on more than one broker, since the ID is not a unique key.

{% hint style="info" %}
When choosing which SQS attributes, Kafka headers or Pub/Sub attributes to filter on, first check whether your framework, messaging client, or observability library already propagates message metadata for you. Many modern stacks can forward tracing-related context out of the box, especially for Kafka headers. Prefer enabling that before adding manual propagation code.

To have your AI agent do this, use the [`mirrord-header-propagation`](https://github.com/metalbear-co/skills/tree/main/skills/mirrord-header-propagation) skill. It adds `baggage` to every message your services publish, restores it in each consumer, and covers Kafka, SQS/SNS, RabbitMQ, GCP Pub/Sub, Azure Service Bus, NATS, Redis Pub/Sub, BullMQ, and Temporal.
{% endhint %}

{% hint style="info" %}
An entry with no `filter`, an empty `message_filter`, and no `jq_filter` is treated as a match-none directive.
{% endhint %}

For complete, copy-pasteable filter examples, see the "Setting a filter" section on each queue service page.
