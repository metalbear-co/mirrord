---
title: NATS
tags:
  - alpha
  - team
  - enterprise
---

This page covers queue splitting for [NATS](https://nats.io). Most of it describes [JetStream](https://docs.nats.io/nats-concepts/jetstream) (persisted streams consumed through a durable pull consumer); [core NATS pub/sub](#core-nats-pubsub-no-jetstream) (plain subject subscriptions) is covered in its own section. For the general concepts and the message filter reference shared by all queue services, see the [Queue Splitting overview](../queue-splitting.md).

Unless the [core pub/sub](#core-nats-pubsub-no-jetstream) section says otherwise, "queue" on this page refers to a JetStream stream consumed through a durable pull consumer.

{% hint style="info" %}
The NATS server must be version `2.2` or later, since splitting relies on message headers.
{% endhint %}

## How It Works

First, we have a consumer app reading messages from a JetStream stream through a durable pull consumer.

When the first mirrord NATS splitting session starts, the operator creates a temporary main-output stream and a durable consumer on it - both deep copies of the originals' settings - and patches the workload's stream and consumer environment variables to point at them. The operator then reads messages through the application's original durable consumer and republishes each one to `<temporary stream>.<original subject>`:

* A message that matches a user's filter is republished to that session's temporary stream, and the local application reads it from there.
* A message that matches no filter is republished to the main-output stream, and the deployed workload reads it from there.

![NATS queue splitting flow](../../.gitbook/assets/nats.svg)

In the default `steal` mode a matched message goes only to the session's temporary stream. In `mirror` mode it goes to both the session's temporary stream and the main output, so the deployed workload also processes a copy.

Delivery is at-least-once: a message is acknowledged on the original consumer only after the JetStream server has acknowledged the republish, so a message can be redelivered but never silently dropped.

When sessions end, their temporary streams are deleted, and when the whole split ends the main-output stream is deleted too. Cleanup is crash-safe: every temporary resource is tracked in a `MirrordClusterExternalResource`, so it is removed even if the operator restarts mid-session.

## Enabling NATS Splitting in Your Cluster

{% stepper %}
{% step %}
#### Enable NATS splitting in the Helm chart

Enable the `operator.natsSplitting` setting in the [mirrord-operator Helm chart](https://github.com/metalbear-co/charts/blob/main/mirrord-operator/values.yaml). This setting covers JetStream splitting, which these steps set up; [core NATS pub/sub](#core-nats-pubsub-no-jetstream) is gated on `operator.natsPubsubSplitting` instead.
{% endstep %}

{% step %}
#### Create a MirrordPropertyList

The operator needs to connect to your NATS server to read and republish messages. Define the connection in a `MirrordPropertyList` ([`CustomResource`](https://kubernetes.io/docs/concepts/extend-kubernetes/api-extension/custom-resources/)) in the same namespace as the target workload (and the `MirrordSplitConfig`), or in the operator's namespace to share it across namespaces - see [Sharing Property Lists Across Namespaces](../queue-splitting.md#sharing-property-lists-across-namespaces).

```yaml
apiVersion: mirrord.metalbear.co/v1
kind: MirrordPropertyList
metadata:
  name: nats-config
  namespace: events
spec:
  properties:
    - name: url
      value: nats://nats.nats-ns.svc.cluster.local:4222
```

Supported properties:

| Property      | Description                                                                                                                | Required | Default |
| ------------- | -------------------------------------------------------------------------------------------------------------------------- | :------: | :-----: |
| `url`         | NATS server URL, e.g. `nats://host:4222`. For a NATS cluster, a comma-separated list of URLs. Use the `tls://` scheme to connect over TLS. | Yes      |         |
| `tls`         | Set to `"true"` to connect over TLS without changing the URL scheme.                                                        | No       | `false` |
| `username`    | Username, used together with `password`.                                                                                    | No       |         |
| `password`    | Password, used together with `username`.                                                                                    | No       |         |
| `token`       | Authentication token.                                                                                                       | No       |         |
| `credentials` | Contents of a JWT `.creds` file.                                                                                            | No       |         |
| `nkey`        | NKey seed.                                                                                                                  | No       |         |
| `ca_cert`     | PEM contents of the CA bundle that signs the server's certificate, for servers with a private CA. Implies TLS.              | No       |         |
| `client_cert` | PEM contents of the client certificate chain, for mTLS. Used together with `client_key`. Implies TLS.                       | No       |         |
| `client_key`  | PEM contents of the client certificate's private key. Used together with `client_cert`.                                     | No       |         |

Set the auth properties matching how your NATS server authenticates clients. Keep secrets in a Kubernetes Secret and reference them with `valueFrom`. Username and password:

```yaml
spec:
  properties:
    - name: url
      value: nats://nats.nats-ns.svc.cluster.local:4222
    - name: username
      value: mirrord-operator
    - name: password
      valueFrom:
        secretKeyRef:
          name: nats-auth
          key: password
```

A token:

```yaml
    - name: token
      valueFrom:
        secretKeyRef:
          name: nats-auth
          key: token
```

A JWT credentials file (the property holds the contents of the `.creds` file, not a path):

```yaml
    - name: credentials
      valueFrom:
        secretKeyRef:
          name: nats-auth
          key: nats.creds
```

An NKey seed:

```yaml
    - name: nkey
      valueFrom:
        secretKeyRef:
          name: nats-auth
          key: nkey.seed
```

TLS with a private CA, or mutual TLS (the properties hold the PEM contents, not paths; providing any of them makes the operator connect over TLS even on a `nats://` URL):

```yaml
    - name: ca_cert
      valueFrom:
        secretKeyRef:
          name: nats-tls
          key: ca.crt
    - name: client_cert
      valueFrom:
        secretKeyRef:
          name: nats-tls
          key: tls.crt
    - name: client_key
      valueFrom:
        secretKeyRef:
          name: nats-tls
          key: tls.key
```

Without `ca_cert`, the server's certificate is verified against the standard public roots, so `client_cert` + `client_key` alone give mTLS against a publicly-trusted server.
{% endstep %}

{% step %}
#### Create a MirrordSplitConfig

On operator installation with `operator.natsSplitting` or `operator.natsPubsubSplitting` enabled, a new [`CustomResource`](https://kubernetes.io/docs/concepts/extend-kubernetes/api-extension/custom-resources/) type is defined in your cluster - `MirrordSplitConfig`. Users with permissions to get CRDs can verify its existence with `kubectl get crd mirrordsplitconfigs.queues.mirrord.metalbear.co`.

Create a `MirrordSplitConfig` for the target workload. NATS uses `kind: nats` in queue entries.

```yaml
apiVersion: queues.mirrord.metalbear.co/v1
kind: MirrordSplitConfig
metadata:
  name: order-processor-split
  namespace: events
spec:
  targetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: order-processor
  clientConfigs:
    nats: nats-config
  queues:
    - id: orders
      kind: nats
      appConfig:
        stream:
          - env: NATS_STREAM
        consumer:
          - env: NATS_CONSUMER
```

The `MirrordSplitConfig` above says that:

1. It targets the deployment `order-processor` in namespace `events`.
2. The NATS connection comes from the `nats-config` `MirrordPropertyList`.
3. The deployment consumes one stream, whose name is in environment variable `NATS_STREAM`, through a durable consumer whose name is in environment variable `NATS_CONSUMER`.
4. The stream can be referenced in a mirrord config under ID `orders`.

#### Link the config to the deployed consumer

The `MirrordSplitConfig` is a namespaced resource. The target workload reference is specified with `spec.targetRef`:

* `apiVersion` - API version of the Kubernetes workload (e.g. `apps/v1`).
* `kind` - type of the workload. Supported: `Deployment`, `StatefulSet`, `Rollout`.
* `name` - name of the workload.

#### Describe consumed streams

Each entry in the `spec.queues` list describes one stream and the durable pull consumer the workload reads it through:

* `id` - arbitrary queue ID that developers [reference](nats.md#setting-a-filter) from their mirrord config.
* `kind` - must be `nats`.
* `clientConfig` (optional) - name of a `MirrordPropertyList` with the NATS connection. Can also be set once for all NATS queues with `spec.clientConfigs.nats`.
* `queueConfig` (optional) - name of a `MirrordPropertyList` with settings for the temporary stream and consumer, see [Configuring temporary streams](nats.md#configuring-temporary-streams).
* `appConfig.stream` - how the application discovers the stream name. Each entry can use the same structure as other queue services: `env`, `envLike`, `volume`, `fallback`, `valuePattern`, `containers`.
* `appConfig.consumer` - how the application discovers the durable consumer name. Uses the same structure as `stream`.

Each queue entry must resolve to exactly one stream and one consumer. If the workload runs several consumers, add one queue entry per consumer.

{% hint style="warning" %}
The mirrord operator can only read consumer's environment variables if they are either:

1. defined directly in the workload's pod template, with the value defined in `value` or in `valueFrom` via config map reference; or
2. loaded from config maps using `envFrom`.
{% endhint %}
{% endstep %}
{% endstepper %}

## Configuring temporary streams

By default the temporary streams and consumers mirrord creates are deep copies of the source stream and consumer, so they inherit their settings - including retention, acknowledgement wait, and replication. You can override these per queue by pointing its `queueConfig` at a `MirrordPropertyList`:

```yaml
apiVersion: mirrord.metalbear.co/v1
kind: MirrordPropertyList
metadata:
  name: orders-queue-config
  namespace: events
spec:
  properties:
    - name: max_age_seconds
      value: "3600"
    - name: ack_wait_seconds
      value: "120"
    - name: num_replicas
      value: "1"
```

Reference it from the queue entry in the `MirrordSplitConfig`:

```yaml
queues:
  - id: orders
    kind: nats
    queueConfig: orders-queue-config
    appConfig:
      stream:
        - env: NATS_STREAM
      consumer:
        - env: NATS_CONSUMER
```

* `max_age_seconds` (integer seconds) - how long the temporary stream keeps messages.
* `ack_wait_seconds` (integer seconds) - how long the temporary consumer waits for an acknowledgement before redelivering a message. Raising it gives your local application more time to handle a message, which is useful when you pause on a breakpoint while debugging.
* `num_replicas` (integer) - how many replicas the temporary stream has.

Each key is optional, and any key you leave out keeps the value copied from the source stream or consumer. An invalid value for a key is ignored (a warning is logged) and that setting falls back to the source's value, so a typo never fails the session.

## Setting a filter

For the full filter reference (`queue_type`, `message_filter`, `jq_filter`), see the [overview](../queue-splitting.md#setting-a-filter-for-a-mirrord-run). NATS uses `queue_type: NATS`.

`message_filter` maps NATS message header names to regexes for their values. A message matches only when **all** entries match, and header name matching is case-sensitive.

Filtering on a message header:

```json
{
  "operator": true,
  "target": "deployment/order-processor/container/consumer",
  "feature": {
    "split_queues": {
      "orders": {
        "queue_type": "NATS",
        "message_filter": {
          "x-tenant": "^test$"
        }
      }
    }
  }
}
```

In the example above, the local application will receive only messages carrying an `x-tenant` header with the value `test`.

`jq_filter` runs a jq program on a JSON object with `subject`, `headers`, and `payload` fields. `payload` is the parsed message body when the body is valid JSON, and a plain string otherwise.

Filtering on the message body with `jq_filter`:

```json
{
  "operator": true,
  "target": "deployment/order-processor/container/consumer",
  "feature": {
    "split_queues": {
      "orders": {
        "queue_type": "NATS",
        "jq_filter": ".payload.user_id == \"test-user\""
      }
    }
  }
}
```

In the example above, the local application will receive only messages whose JSON body contains `"user_id": "test-user"`.

{% hint style="info" %}
When the operator's `operator.injectSessionKeyHeader` setting is enabled, every message delivered to your session carries a `mirrord-key` header with your session key - see [Session Key Header](../queue-splitting.md#session-key-header).
{% endhint %}

## Core NATS pub/sub (no JetStream)

Applications that subscribe to plain subjects (`nc.Subscribe(...)`, no streams, no durables) can be split with the separate `natsPubSub` queue kind. The operator subscribes to the original subject and republishes each message under a temporary subject prefix - the session's on a filter match, the workload's fallback prefix otherwise. The application's subject value is rewritten to `<prefix>.<original>`, so wildcard subscriptions (`orders.*`, `orders.>`) keep working unchanged.

{% hint style="warning" %}
Core NATS stores nothing, so delivery is **best-effort**: messages published while the split is being set up, torn down, or while the operator is briefly unavailable are not replayed. Applications that must not miss messages belong on JetStream, covered by the sections above.
{% endhint %}

Core NATS pub/sub splitting requires operator and Helm chart `3.205.0` or later, and mirrord CLI `3.256.0` or later.

Enable it with `operator.natsPubsubSplitting: true` in the Helm chart. The operator still connects to your NATS server through a `MirrordPropertyList`, created exactly as in [Create a MirrordPropertyList](#create-a-mirrordpropertylist) above and referenced from the split config's `clientConfigs.nats` (same server, same credentials as JetStream splitting). The `MirrordSplitConfig` entry uses `kind: natsPubSub` and a `subject` reference:

```yaml
  queues:
    - id: orders-pubsub
      kind: natsPubSub
      appConfig:
        subject:
          - env: NATS_SUBJECT
```

In the mirrord configuration, filters use `queue_type: NATSPubSub` with the same `message_filter` (headers) and `jq_filter` (`subject` / `headers` / `payload` document) as JetStream splitting. No temporary resources exist on the server - subjects are just names - so there is nothing to clean up when sessions end beyond restoring the workload's env.

## Notes and limitations

* Except for the [Core NATS pub/sub](#core-nats-pubsub-no-jetstream) section, this page covers JetStream: the application must consume through a durable pull consumer on a stream. Plain subject subscriptions are split with best-effort delivery instead.
* The NATS server must be version `2.2` or later, since splitting relies on message headers.
* Each queue entry in the `MirrordSplitConfig` describes exactly one stream and one consumer. Add one entry per consumer.
* Republished messages carry the subject `<temporary stream>.<original subject>` - the original subject is kept, prefixed with the temporary stream's name. An application that routes on exact subjects sees the prefixed subject while a split is active, so match on the subject's suffix (or a wildcard) instead of the full subject.
* Delivery is at-least-once: a message is acknowledged on the original consumer only after the JetStream server acknowledged the republish, so your application may see a message twice but never miss one.
