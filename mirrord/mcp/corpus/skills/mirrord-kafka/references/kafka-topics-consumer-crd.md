# MirrordKafkaTopicsConsumer CRD Reference (DEPRECATED)

> **⚠️ Deprecated.** `MirrordKafkaTopicsConsumer` is replaced by **`MirrordSplitConfig`** (`references/mirrord-split-config-crd.md`). Existing resources still work. Use this file only when helping maintain or migrate a legacy setup; generate `MirrordSplitConfig` for anything new. The reference file contains the old→new field mapping.

## Overview
Links a Kubernetes workload (Deployment, StatefulSet, or Argo Rollout) to the Kafka topics it consumes, providing the context the mirrord Operator needs to perform queue splitting. Must be created in the **same namespace** as the target workload.

**API Version:** `queues.mirrord.metalbear.co/v1alpha`  
**Kind:** `MirrordKafkaTopicsConsumer`

## Top-level spec fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `consumerApiVersion` | string | Yes | K8s API version of the workload (e.g. `apps/v1`) |
| `consumerKind` | string | Yes | Workload kind: `Deployment`, `StatefulSet`, or `Rollout` (Argo) |
| `consumerName` | string | Yes | Name of the K8s workload |
| `topics` | list of topic entries | Yes | Kafka topics consumed by this workload |
| `consumerRestartTimeout` | integer (seconds) | No | How long to wait for new pod readiness after restart. Default: 60 |
| `splitTtl` | integer (seconds) | No | How long to keep workload patched after last session ends. Avoids restart if a new session starts soon. |

## topics[] entry

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | string | Yes | Arbitrary ID referenced from mirrord.json `feature.split_queues.<id>` |
| `clientConfig` | string | Yes | Name of a `MirrordKafkaClientConfig` resource (in operator namespace) |
| `nameSources` | list | Yes | Where the topic name is found in the workload's pod spec |
| `groupIdSources` | list | **Yes** (required!) | Where the consumer group ID is found in the workload's pod spec |

### nameSources[] / groupIdSources[] entry

Each entry describes an environment variable source. Supported types:

#### directEnvVar
```yaml
- directEnvVar:
    container: consumer     # container name in the pod spec
    variable: KAFKA_TOPIC   # env var name
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `container` | string | Yes | Container name in the pod template |
| `variable` | string | Yes | Environment variable name |

## Important constraints

- **Namespace:** Must be in the same namespace as the target workload
- **groupIdSources is REQUIRED:** The CRD schema currently marks it optional, but omitting it causes a 500 error from the operator. Always include it.
- **Env var readability:** The operator can only read env vars that are either:
  1. Defined directly in the pod template with `value` or `valueFrom` (configMapKeyRef)
  2. Loaded from ConfigMaps using `envFrom`
  - Vault-injected env vars are NOT currently supported
- **Supported workload types:** Deployment, StatefulSet, Argo Rollout

## Full example

```yaml
apiVersion: queues.mirrord.metalbear.co/v1alpha
kind: MirrordKafkaTopicsConsumer
metadata:
  name: my-app-topics-consumer
  namespace: my-namespace
spec:
  consumerApiVersion: apps/v1
  consumerKind: Deployment
  consumerName: my-app
  consumerRestartTimeout: 120
  splitTtl: 300
  topics:
  - id: orders-topic
    clientConfig: base-config
    nameSources:
    - directEnvVar:
        container: app
        variable: KAFKA_TOPIC_NAME
    groupIdSources:
    - directEnvVar:
        container: app
        variable: KAFKA_GROUP_ID
  - id: events-topic
    clientConfig: base-config
    nameSources:
    - directEnvVar:
        container: app
        variable: EVENTS_TOPIC
    groupIdSources:
    - directEnvVar:
        container: app
        variable: EVENTS_GROUP_ID
```

## How topic IDs connect to mirrord.json

The `id` field in each topic entry is what developers reference in their mirrord.json:

```json
{
  "feature": {
    "split_queues": {
      "orders-topic": {
        "queue_type": "Kafka",
        "message_filter": {
          "baggage": ".*mirrord-session=alice.*"
        }
      }
    }
  }
}
```

The topic ID `orders-topic` must match between the `MirrordKafkaTopicsConsumer` and the mirrord config.
