---
title: RabbitMQ
tags:
  - alpha
  - team
  - enterprise
---

This page covers queue splitting for [RabbitMQ](https://www.rabbitmq.com). For the general concepts and the message filter reference shared by all queue services, see the [Queue Splitting overview](../queue-splitting.md).

{% hint style="info" %}
Queue splitting via `MirrordSplitConfig` requires mirrord operator `3.170.0` or later and mirrord CLI `3.221.0` or later.
{% endhint %}

{% hint style="warning" %}
**⚠️ Deprecated CRD**

`MirrordWorkloadQueueRegistry` is deprecated and replaced by `MirrordSplitConfig`. Existing resources continue to work for backward compatibility, but we recommend migrating to `MirrordSplitConfig`. See [Migrating to MirrordSplitConfig](migrating-to-mirrordsplitconfig.md#rabbitmq).
{% endhint %}

## How It Works

First, we have a consumer app reading messages from a RabbitMQ queue:

![A K8s application that consumes messages from a RabbitMQ queue](../../.gitbook/assets/before-splitting-rmq.svg)

When the first mirrord RabbitMQ splitting session starts, two temporary queues are created (one for the target deployed in the cluster, one for the user's local application), and the mirrord operator routes messages according to the [user's filter](rabbitmq.md#setting-a-filter):

![One RabbitMQ splitting session](../../.gitbook/assets/1-user-rmq.svg)

If a second user then starts a mirrord RabbitMQ splitting session on the same queue, a third temporary queue is created (for the second user's local application). The mirrord operator includes the new queue and the second user's filter in the routing logic.

![Two RabbitMQ splitting sessions](../../.gitbook/assets/2-users-rmq.svg)

If the filters defined by the two users both match some message, one of the users will receive the messages at random.

## Enabling RabbitMQ Splitting in Your Cluster

{% stepper %}
{% step %}
#### Enable RabbitMQ splitting in the Helm chart

Enable the `operator.rmqSplitting` setting in the [mirrord-operator Helm chart](https://github.com/metalbear-co/charts/blob/main/mirrord-operator/values.yaml).
{% endstep %}

{% step %}
#### Cluster Declaration

The mirrord operator needs a way to connect to your RabbitMQ cluster to consume and re-route messages according to filters. As part of operator installation with `operator.rmqSplitting` enabled, a new [`CustomResource`](https://kubernetes.io/docs/concepts/extend-kubernetes/api-extension/custom-resources/) type is defined in your cluster — `MirrordPropertyList`. Use this resource to define the cluster and queue connection parameters for splitting. A `MirrordPropertyList` lives in the same namespace as the consumer workload (and the `MirrordSplitConfig`), which may very well be different than your RabbitMQ broker's namespace. It can also live in the operator's namespace to share one declaration across namespaces - see [Sharing Property Lists Across Namespaces](../queue-splitting.md#sharing-property-lists-across-namespaces). `MirrordPropertyList` is modeled after the `env` and `envFrom` fields in a pod's container spec. You can:

* Set values directly in the `properties` field using `value`.
* Reference a single key from a ConfigMap or Secret using `valueFrom.configMapKeyRef` or `valueFrom.secretKeyRef`.
* Include all keys from a ConfigMap or Secret using `configMapRef` or `secretRef` under `propertiesFrom`. An optional `prefix` is prepended to each key.

{% hint style="warning" %}
If you set `properties` field using `value` then it must always be string `value: '1'` instead of `value: 1`.
{% endhint %}

```yaml
apiVersion: mirrord.metalbear.co/v1
kind: MirrordPropertyList
metadata:
  name: meme-rmq-cluster
  namespace: meme
spec:
  properties:
    - name: host
      value: meme-rmq.meme.svc
    - name: username
      valueFrom:
        configMapKeyRef:
          name: meme-rmq-config
          key: rmq_user
    - name: password
      valueFrom:
        secretKeyRef:
          name: meme-rmq-secret
          key: rmq_password
  propertiesFrom:
    - secretRef:
        prefix: 'client.'
        name: meme-rmq-client-properties
        optional: true
    - configMapRef:
        name: meme-rmq-common-properties
        optional: true
```

You must create at least one `MirrordPropertyList` with your cluster properties inside of it.

{% hint style="info" %}
If your application expects specific queue attributes (e.g. `durable`, or arguments like `x-queue-type`), create a MirrordPropertyList with those queue declaration properties.

```yaml
apiVersion: mirrord.metalbear.co/v1
kind: MirrordPropertyList
metadata:
  name: meme-quorum-queue
  namespace: meme
spec:
  properties:
    - name: durable
      value: 'true'
    - name: arguments.x-queue-type
      value: quorum
```
{% endhint %}

#### Cluster Properties

| Property              |                                                                                                             Description                                                                                                            | Required |                              Type                             |               Default              |
| --------------------- | :--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------: | :------: | :-----------------------------------------------------------: | :--------------------------------: |
| `url`                 | Full AMQP connection URL (e.g. `amqp://user:pass@host:5672/vhost`), used as an alternative to `host`. Any explicitly set `scheme`, `port`, `username`, `password`, or `vhost` overrides the corresponding part parsed from the URL |    ✓ ¹   |                          string (URI)                         |                                    |
| `scheme`              |                                                                                                  Protocol used for the connection                                                                                                  |          |                       `amqp` or `amqps`                       |               `amqp`               |
| `host`                |                                                                                            Hostname or IP address of the message broker                                                                                            |    ✓ ¹   |                             string                            |                                    |
| `port`                |                                                                                               Network port the broker is listening on                                                                                              |          |                            integer                            | 5671 or 5672 according to `scheme` |
| `username`            |                                                                                           Credential used to authenticate the connection                                                                                           |          |                             string                            |                                    |
| `password`            |                                                                                            Secret key or password for the specified user                                                                                           |          |                             string                            |                                    |
| `vhost`               |                                                                                      A logical isolation unit (virtual host) within the broker                                                                                     |          |                             string                            |                 '/'                |
| `sasl.mechanism`      |                                                                                          Authentication strategy used during the handshake                                                                                         |          | `amqplain` `anonymous` `external` `plain` or `rabbit-cr-demo` |                                    |
| `tls.crt`             |                                                                                   public certificate (PEM format) used for client authentication                                                                                   |          |                          string (PEM)                         |                                    |
| `tls.key`             |                                                                                      private key (PEM format) matching the client certificate                                                                                      |          |                          string (PEM)                         |                                    |
| `ca-certificates.crt` |                                                                                 CA certificate(s) (PEM format) used to verify the broker's identity                                                                                |          |                          string (PEM)                         |                                    |
| `client.*`            |                                                                                          Custom metadata or properties sent to the broker                                                                                          |          |                    object / key-value pairs                   |                                    |

¹ Provide either `url` or `host`. Whenever a part is both present in the `url` and set as its own property, the individual property wins. A `username` and `password` are always required - set them directly or include them in the `url`.

#### Queue Declare Properties

| Property      |                                                    Description                                                    | Required |           Type           | Default |
| ------------- | :---------------------------------------------------------------------------------------------------------------: | :------: | :----------------------: | :-----: |
| `durable`     |                                    If true, the queue survives a broker restart                                   |          |          boolean         |  false  |
| `exclusive`   | If true, the queue can only be accessed by the current connection and will be deleted when that connection closes |          |          boolean         |  false  |
| `auto_delete` |                  If true, the queue is deleted automatically when the last consumer unsubscribes                  |          |          boolean         |  false  |
| `arguments.*` |                                    Custom properties sent in queue declaration                                    |          | object / key-value pairs |         |
{% endstep %}

{% step %}
#### Provide application context

On operator installation with `operator.rmqSplitting` enabled, a new [`CustomResource`](https://kubernetes.io/docs/concepts/extend-kubernetes/api-extension/custom-resources/) type is defined in your cluster — `MirrordSplitConfig`. Users with permissions to get CRDs can verify its existence with `kubectl get crd mirrordsplitconfigs.queues.mirrord.metalbear.co`. Before you can run sessions with RabbitMQ splitting, you must create a `MirrordSplitConfig` for the desired target. This tells the operator which queues to split and how the application discovers their names.

See an example `MirrordSplitConfig` defined for a deployment `meme-app` living in namespace `meme`:

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

The `MirrordSplitConfig` above says that:

1. It targets container `main` running in deployment `meme-app` in namespace `meme`.
2. The cluster connection parameters are defined in the `meme-rmq-cluster` `MirrordPropertyList`, looked up in the same namespace as this config (`meme`) and then in the operator's namespace.
3. The container consumes two RabbitMQ queues. Their names are read from environment variables `INCOMING_MEME_QUEUE_NAME` and `AD_QUEUE_NAME`.
4. The queues can be referenced in a mirrord config under IDs `meme-queue` and `ad-queue`, respectively.

#### Link the config to the deployed consumer

The `MirrordSplitConfig` is a namespaced resource, so it can only reference a consumer deployed in the same namespace. The target workload reference is specified with `spec.targetRef`:

* `apiVersion` — API version of the Kubernetes workload (e.g. `apps/v1`, or `argoproj.io/v1alpha1` for rollouts).
* `kind` — type of the workload. Supported: `Deployment`, `StatefulSet`, `Rollout`.
* `name` — name of the workload.

#### Describe consumed queues

Each entry in the `spec.queues` list describes one or more RabbitMQ queues consumed by the workload:

* `id` — arbitrary queue ID that developers [reference](rabbitmq.md#setting-a-filter) from their mirrord config.
* `kind` — must be `rmq`.
* `clientConfig` (required for RabbitMQ) — name of the `MirrordPropertyList` containing connection properties for the RabbitMQ cluster.
* `queueConfig` (optional) — name of a `MirrordPropertyList` that contains queue declaration properties (durable, queue type, or any other attribute) for the temporary queues.
* `appConfig.queue` — how the application discovers the queue name. Each entry can use:
  * `env` — exact environment variable name containing the queue name.
  * `envLike` — regex matching environment variable names.
  * `volume` - read the queue name from a file mounted from a `configMap` volume (`volume.name` + `volume.file`) instead of an environment variable. See [Queue Names in Mounted Config Files](../queue-splitting.md#queue-names-in-mounted-config-files).
  * `fallback` — fallback queue name if the variable is not found. The env var is still rewritten to point at the temporary queue.
  * `valueSelector` — a selector extracting the queue name from the variable's value: nested keys and `.[]` to iterate arrays or object values. Useful when the env var holds JSON rather than a plain name. Pipes, functions, and other jq operators are not supported.
  * `valuePattern` — a regex used when the queue name is embedded in a larger string. The capture group (named `value`, otherwise the first group) marks the part that is the name; only that part is swapped for the temporary queue and the surrounding text is kept as-is.
  * `containers` — limit to specific containers (optional, defaults to all non-infra containers).
* `appConfig.exchange` (optional) — when the application reads an exchange name from the environment, the operator injects a dummy exchange name there so the local app does not publish to or bind against the real exchange. Uses the same structure as `appConfig.queue`.

{% hint style="warning" %}
The mirrord operator can only read consumer's environment variables if they are either:

1. defined directly in the workload's pod template, with the value defined in `value` or in `valueFrom` via config map reference; or
2. loaded from config maps using `envFrom`.
{% endhint %}
{% endstep %}
{% endstepper %}

## Drain timeout

After the last session against a target ends, the operator keeps the split's temporary resources alive for the drain timeout so a new session can reuse them, then tears them down. It does not wait for unread messages to be consumed first.

| Setting                                         | Unit    | Scope     | Effect                              |
| ----------------------------------------------- | ------- | --------- | ----------------------------------- |
| `spec.drainTimeout` on the `MirrordSplitConfig` | seconds | One split | Wins over the cluster-wide default. |

| `drainTimeout` | Behavior                                                  |
| -------------- | --------------------------------------------------------- |
| unset (both)   | Tear down as soon as the last session ends (same as `0`). |
| `0`            | Tear down immediately. Unread messages may be lost.       |
| `N`            | Keep resources for up to `N` seconds, then tear down.     |

## Setting a filter

For the full filter reference (`queue_type`, `message_filter`, `jq_filter`), see the [overview](../queue-splitting.md#setting-a-filter-for-a-mirrord-run). RabbitMQ uses `queue_type: RMQ`, and supports both `message_filter` on message headers and `jq_filter` on the whole message.

```json
{
  "operator": true,
  "target": "deployment/meme-app/container/main",
  "feature": {
    "split_queues": {
      "meme-queue": {
        "queue_type": "RMQ",
        "message_filter": {
          "baggage": ".*mirrord-session=alice.*"
        }
      }
    }
  }
}
```

In the example above, the local application will receive a subset of messages from the RabbitMQ queue described in the `MirrordSplitConfig` under ID `meme-queue`. All received messages will have a message header `baggage` containing `mirrord-session=alice`.

### Filtering with jq

{% hint style="info" %}
JQ filters on RabbitMQ require operator version `>=3.201.0` and CLI version `>=3.253.0`
{% endhint %}

`message_filter` only matches individual headers by name. Use `jq_filter` to match on the message body, or on headers whose name you don't know in advance. The jq program runs on this JSON document:

```json
{
  "headers": {
    "baggage": "mirrord-session=alice"
  },
  "properties": {
    "content_type": "application/json",
    "message_id": "5f2c...",
    "timestamp": 1755600000
  },
  "payload": "{\"priority\":\"high\"}"
}
```

* `headers` is the AMQP basic-properties headers table. It is always present, and may be empty.
* `properties` holds the remaining basic properties that carry a value: `content_type`, `content_encoding`, `delivery_mode`, `priority`, `correlation_id`, `reply_to`, `expiration`, `message_id`, `timestamp`, `type`, `user_id`, and `app_id`. Properties the publisher did not set are absent.
* `payload` is the message body.

A message matches when the program outputs `true`. To filter on the body, parse it with `fromjson` first:

```json
{
  "operator": true,
  "target": "deployment/meme-app/container/main",
  "feature": {
    "split_queues": {
      "meme-queue": {
        "queue_type": "RMQ",
        "jq_filter": ".payload | fromjson | .priority == \"high\""
      }
    }
  }
}
```

In the example above, the local application receives only messages whose body is a JSON object with `"priority": "high"`. Messages whose body is not valid JSON never match.

If both `message_filter` and `jq_filter` are set for a queue, a message must match both. The header filter runs first, so the jq program only sees messages that already passed it.
