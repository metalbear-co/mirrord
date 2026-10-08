---
title: Kafka
tags:
  - beta
  - team
  - enterprise
---

This page covers queue splitting for [Kafka](https://kafka.apache.org/). For the general concepts and the message filter reference shared by all queue services, see the [Queue Splitting overview](../queue-splitting.md).

The word "queue" on this page refers to a Kafka topic.

{% hint style="info" %}
Queue splitting via `MirrordSplitConfig` requires mirrord operator `3.170.0` or later and mirrord CLI `3.221.0` or later.
{% endhint %}

{% hint style="warning" %}
**⚠️ Deprecated CRD**

`MirrordKafkaTopicsConsumer` + `MirrordKafkaClientConfig` are deprecated and replaced by `MirrordSplitConfig`. Existing resources continue to work for backward compatibility, but we recommend migrating to `MirrordSplitConfig`. See [Migrating to MirrordSplitConfig](migrating-to-mirrordsplitconfig.md#kafka).

The older `operator.idleKafkaSplitTtlMillis` Helm value (`OPERATOR_KAFKA_SPLITTING_TTL`) only affects legacy `MirrordKafkaTopicsConsumer` objects that leave `spec.splitTtl` unset. It is not a cluster-wide default for `MirrordSplitConfig`, and there is no cluster-wide equivalent: set [`spec.ttl` and `spec.drainTimeout`](kafka.md#configuring-workload-restart) on each config instead.
{% endhint %}

## How It Works

First, we have a consumer app reading messages from a Kafka queue:

![A K8s application that consumes messages from a Kafka queue](../../.gitbook/assets/before-splitting-kafka.svg)

When the first mirrord Kafka splitting session starts, two temporary queues are created (one for the target deployed in the cluster, one for the user's local application), and the mirrord operator routes messages according to the [user's filter](kafka.md#setting-a-filter):

![One Kafka splitting session](../../.gitbook/assets/1-user-kafka.svg)

If a second user then starts a mirrord Kafka splitting session on the same queue, a third temporary queue is created (for the second user's local application). The mirrord operator includes the new queue and the second user's filter in the routing logic.

![Two Kafka splitting sessions](../../.gitbook/assets/2-users-kafka.svg)

If the filters defined by the two users both match some message, one of the users will receive the message at random.

## Enabling Kafka Splitting in Your Cluster

{% stepper %}
{% step %}
#### Enable Kafka splitting in the Helm chart

Enable the `operator.kafkaSplitting` setting in the [mirrord-operator Helm chart](https://github.com/metalbear-co/charts/blob/main/mirrord-operator/values.yaml).
{% endstep %}

{% step %}
#### Configure the operator's Kafka client

The mirrord operator needs to be able to perform some operations on the Kafka cluster. The connection settings live in a `MirrordPropertyList` ([`CustomResource`](https://kubernetes.io/docs/concepts/extend-kubernetes/api-extension/custom-resources/)), which you reference from the `MirrordSplitConfig` (see the next step).

The `MirrordPropertyList` lives in the **same namespace as the target workload** (and the `MirrordSplitConfig`), or in the operator's namespace to share one client config across namespaces - see [Sharing Property Lists Across Namespaces](../queue-splitting.md#sharing-property-lists-across-namespaces). Each property is a Kafka client property; the operator passes them straight to the underlying Kafka client. A few operator-specific keys (prefixed with `mirrord.`) are consumed by the operator and not forwarded to the client.

```yaml
apiVersion: mirrord.metalbear.co/v1
kind: MirrordPropertyList
metadata:
  name: kafka-connection
  namespace: meme
spec:
  properties:
    - name: bootstrap.servers
      value: kafka.default.svc.cluster.local:9092
    - name: security.protocol
      value: PLAINTEXT
```

The full list of available Kafka client properties can be found [here](https://github.com/confluentinc/librdkafka/blob/master/CONFIGURATION.md).

The operator recognizes these `mirrord.`-prefixed keys:

* `mirrord.client_implementation` - the Kafka client backend, `librdkafka` (default) or `java`. Use `java` for Kafka Streams consumers (see below).
* `mirrord.auth.kind` - extra authentication mechanism. The only supported value is `MSK_IAM` (see [MSK IAM authentication](kafka.md#aws-msk-iam-authentication)).
* `mirrord.auth.aws_region` - the AWS region, required when `mirrord.auth.kind` is `MSK_IAM`.
* `mirrord.split_topic.replication_factor` - the replication factor for temporary topics (see [Temporary Topic Replication Factor](kafka.md#temporary-topic-replication-factor)).
* `mirrord.ssl.truststore.base64`, `mirrord.ssl.keystore.base64`, `mirrord.ssl.keystore.alias` - TLS credentials held in Java KeyStores (see [authenticating with a Java KeyStore](kafka.md#how-do-i-authenticate-the-operators-kafka-client-with-a-java-keystore)).

{% hint style="info" %}
The Kafka consumer group used by the operator's own client is managed by mirrord, so a `group.id` property is not needed here.
{% endhint %}

{% hint style="info" %}
If no `MirrordPropertyList` with the referenced name exists in the target's namespace, the operator falls back to a legacy `MirrordKafkaClientConfig` of the same name in the operator's namespace. This keeps older setups working untouched. See [Migrating to MirrordSplitConfig](migrating-to-mirrordsplitconfig.md#kafka).
{% endhint %}
{% endstep %}

{% step %}
#### Authorize deployed consumers

In order to be targeted with Kafka splitting, a deployed consumer must be able to use the temporary queues created by mirrord. E.g. if the consumer application describes the queue or reads messages from it — it must be able to do the same on a temporary queue. This might require extra actions on your side to adjust the authorization, for example based on queue name prefix. See [customizing temporary queue names](kafka.md#customizing-temporary-kafka-queue-names) for more info.
{% endstep %}

{% step %}
#### Provide application context

On operator installation with `operator.kafkaSplitting` enabled, a new [`CustomResource`](https://kubernetes.io/docs/concepts/extend-kubernetes/api-extension/custom-resources/) type is defined in your cluster - `MirrordSplitConfig`. Users with permissions to get CRDs can verify its existence with `kubectl get crd mirrordsplitconfigs.queues.mirrord.metalbear.co`. Before you can run sessions with Kafka splitting, you must create a `MirrordSplitConfig` for the desired target. This tells the operator which queues to split and how the application discovers their names.

See an example `MirrordSplitConfig` for a meme app that consumes messages from a Kafka queue:

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
            fallback: views-topic # optional, used when the variable is absent
            containers:
              - consumer
        groupId:
          - env: KAFKA_GROUP_ID
            containers:
              - consumer
```

The `MirrordSplitConfig` above says that:

1. It targets the deployment `meme-app` in namespace `meme`.
2. The deployment consumes one queue. Its name is read from environment variable `KAFKA_TOPIC_NAME` in container `consumer`. If the variable is absent, the fallback value `views-topic` is used instead. The Kafka consumer group id is read from environment variable `KAFKA_GROUP_ID` in container `consumer`.
3. The Kafka queue can be referenced in a mirrord config under ID `views-topic`.
4. The Kafka client connection comes from the `kafka-connection` `MirrordPropertyList`.

#### Link the config to the deployed consumer

The `MirrordSplitConfig` is a namespaced resource, so it can only reference a consumer deployed in the same namespace. The target workload reference is specified with `spec.targetRef`:

* `apiVersion` - API version of the Kubernetes workload (e.g. `apps/v1`, or `argoproj.io/v1alpha1` for rollouts).
* `kind` - type of the workload. The operator supports Kafka splitting on deployments, stateful sets, and Argo rollouts.
* `name` - name of the workload.

#### Describe consumed queues

Each entry in the `spec.queues` list describes one or more Kafka queues consumed by the workload:

* `id` - arbitrary queue ID that developers [reference](kafka.md#setting-a-filter) from their mirrord config.
* `kind` - must be `kafka`.
* `clientConfig` - name of the `MirrordPropertyList` with the Kafka client connection (from the previous step). Can also be set once for all Kafka queues with `spec.clientConfigs.kafka`.
* `appConfig.topic` - how the application discovers the topic name. Each entry can use:
  * `env` - exact environment variable name containing the topic name.
  * `envLike` - regex matching environment variable names.
  * `volume` - read the topic name from a file mounted from a `configMap` volume (`volume.name` + `volume.file`) instead of an environment variable. See [Queue Names in Mounted Config Files](../queue-splitting.md#queue-names-in-mounted-config-files).
  * `fallback` - fallback topic name if the variable is absent (only valid with `env`). The env var is still rewritten to point at the temporary topic.
  * `valueSelector` - a selector extracting the topic name from the variable's value: nested keys (`.kafka.topic`) and `.[]` to iterate arrays or object values. Useful when the value holds JSON rather than a plain name. Pipes, functions, and other jq operators are not supported.
  * `valuePattern` - a regex used when the topic name is embedded in a larger string. The capture group (named `value`, otherwise the first group) marks the part that is the name; only that part is swapped for the temporary topic and the surrounding text is kept as-is.
  * `containers` - limit to specific containers (optional, defaults to all non-infra containers).
*   One of the following must be set:

    * `appConfig.groupId` - how the application discovers the consumer Kafka group id. Use for standard Kafka consumers. The operator's forwarder joins this group, and the consumer's environment is left untouched.
    * `appConfig.appId` - how the application discovers the Kafka Streams application id. Use for Kafka Streams consumers. The operator patches this variable to a fresh application id. Kafka Streams requires the Java client (`mirrord.client_implementation: java`).

    Both use the same structure as `appConfig.topic`.

{% hint style="warning" %}
The mirrord operator can only read consumer's environment variables if they are either:

1. defined directly in the workload's pod template, with the value defined in `value` or in `valueFrom` via config map reference; or
2. loaded from config maps using `envFrom`.
{% endhint %}

If the consumer reads its topic and group from a mounted config file instead of environment variables, point the sources at the file:

```yaml
appConfig:
  topic:
    - volume:
        name: app-config          # configMap volume in the pod spec
        file: application.yaml
      valueSelector: ".kafka.consumer.topic.main.name"
  groupId:
    - volume:
        name: app-config
        file: application.yaml
      valueSelector: ".kafka.consumer.group"
```

See [Queue Names in Mounted Config Files](../queue-splitting.md#queue-names-in-mounted-config-files) for how the operator rewrites the file for the deployed and local applications.
{% endstep %}
{% endstepper %}

## Additional Options

### Customizing Temporary Kafka Queue Names

To serve Kafka splitting sessions, the mirrord operator creates temporary queues in the Kafka cluster. The default format for their names is as follows:

* `mirrord-tmp-1234567890-fallback-topic-original-topic` - for the fallback queue (unfiltered messages, consumed by the deployed workload).
* `mirrord-tmp-0987654321-original-topic` - for the user queues (filtered messages, consumed by local applications running with mirrord).

Note that the random characters will be unique for each temporary queue created by the operator.

You can adjust the format of the created queue names to suit your needs (RBAC, security, policies, etc.), using the `OPERATOR_KAFKA_SPLITTING_TOPIC_FORMAT` environment variable of the mirrord operator, or the `operator.kafkaSplittingTopicFormat` helm chart value. The default value is:

`mirrord-tmp-{{RANDOM}}{{FALLBACK}}{{ORIGINAL_TOPIC}}`

The provided format must contain the three variables: `{{RANDOM}}`, `{{FALLBACK}}` and `{{ORIGINAL_TOPIC}}`.

* `{{RANDOM}}` will resolve to random characters.
* `{{FALLBACK}}` will resolve either to `-fallback-` or `-` literal.
* `{{ORIGINAL_TOPIC}}` will resolve to the name of the original topic that is being split.

### Configuring Workload Restart

To inject the names of the temporary queues into the consumer workload, the operator always requires the workload to be restarted. Depending on cluster conditions, and the workload itself, this might take some time.

`MirrordSplitConfig` lets you tune this with a few optional fields:

```yaml
spec:
  restart:
    timeout: 120
  ttl: 60
  drainTimeout: 300
```

`spec.restart.timeout` is how long the operator waits for a new pod to become ready after the workload restart is triggered (in seconds, defaults to 60). This silences timeout errors when the workload pods take a long time to start.

When the **last** splitting session on a workload ends, the operator does not tear the split down right away. It passes through two windows before deleting the temporary queues and unpatching the workload (which restarts it back onto the original topic):

1. **Idle window - `spec.ttl`.** The split stays fully live: the operator keeps forwarding the original topic into the temporary topic the patched workload reads. A session that reconnects during this window reuses the split **instantly**, with no restart. This is the "keep it warm" TTL.
2. **Drain window - limited by `spec.drainTimeout`.** Once the idle window elapses with no reconnect, the operator stops forwarding new messages and lets the patched workload finish consuming what is already in the temporary topic. A session that reconnects during this window reuses the same split and resumes forwarding instead of rebuilding from scratch. The window ends **early** the moment that topic is fully consumed, and is capped at `drainTimeout`.

Both fields are optional and in seconds:

| Field | Behavior |
| ----- | -------- |
| `spec.ttl` | `N`: keep the split warm for up to `N` seconds so a reconnecting session resumes instantly. `0` or unset: do not linger - go straight to the drain window when the last session ends. |
| `spec.drainTimeout` | `N`: let the workload finish the already-forwarded backlog for up to `N` seconds, ending early once it is drained. `0`: unpatch immediately - messages not yet read from the temporary topic are lost. Unset: no cap - wait until the workload has consumed the backlog. |

{% hint style="info" %}
`spec.ttl`, and draining the temporary topic before unpatch (capped by `spec.drainTimeout`), require mirrord operator `3.194.0` or later. On earlier operators `spec.drainTimeout` alone controls how long the workload stays patched after the last session.
{% endhint %}

### Temporary Topic Replication Factor

By default, the operator creates temporary topics with a replication factor of 1. Some managed Kafka platforms enforce a minimum replication factor and reject these topics - for example, Confluent Cloud requires a factor of 3, so Kafka splitting sessions fail with a `PolicyViolation` broker error.

Set the `mirrord.split_topic.replication_factor` property on the `MirrordPropertyList` to control the factor. This property requires mirrord operator `3.191.0` or later; earlier operators reject it as an unknown `mirrord.` key.

```yaml
apiVersion: mirrord.metalbear.co/v1
kind: MirrordPropertyList
metadata:
  name: kafka-connection
  namespace: meme
spec:
  properties:
    - name: bootstrap.servers
      value: kafka.default.svc.cluster.local:9092
    - name: mirrord.split_topic.replication_factor
      value: copy
```

Accepted values:

* a positive number - used as-is for every temporary topic.
* `copy` - copy the replication factor derived from the original topic. This is recommended when temporary topics must use the same replication factor as source topics on managed platforms like Confluent Cloud.
* `-1` - use the broker's default replication factor.

### AWS MSK IAM authentication

For [Amazon Managed Streaming for Apache Kafka](https://aws.amazon.com/msk/) with IAM/OAUTHBEARER authentication, set the operator keys on the `MirrordPropertyList`:

```yaml
apiVersion: mirrord.metalbear.co/v1
kind: MirrordPropertyList
metadata:
  name: kafka-connection
  namespace: meme
spec:
  properties:
    - name: bootstrap.servers
      value: b-1.mycluster.kafka.eu-south-1.amazonaws.com:9098
    - name: mirrord.auth.kind
      value: MSK_IAM
    - name: mirrord.auth.aws_region
      value: eu-south-1
```

When `mirrord.auth.kind` is `MSK_IAM`, the operator automatically adds `sasl.mechanism=OAUTHBEARER` and `security.protocol=SASL_SSL`.

To produce the authentication tokens, the operator uses the default credentials provider chain. The easiest way to provide the credentials is with IAM role assumption. For that, an IAM role with an appropriate policy has to be assigned to the operator's service account. Please follow [AWS's documentation on how to do that](https://docs.aws.amazon.com/eks/latest/userguide/associate-service-account-role.html). Note that the operator's service account can be annotated with the IAM role's ARN with the `sa.roleArn` setting in the [mirrord-operator Helm chart](https://github.com/metalbear-co/charts/blob/main/mirrord-operator/values.yaml).

### KafkaJS and other clients with custom group protocols (`mirrord.temporary_group_id`)

This is useful when the split session fails with an `INCONSISTENT_GROUP_PROTOCOL` error. It happens with client libraries that advertise their own partition-assignment protocol names - KafkaJS, for example - which the operator's consumer cannot join a group with.

Set the `mirrord.temporary_group_id` property on the Kafka `MirrordPropertyList`:

```yaml
apiVersion: mirrord.metalbear.co/v1
kind: MirrordPropertyList
metadata:
  name: kafka-connection
  namespace: meme
spec:
  properties:
    - name: bootstrap.servers
      value: kafka.default.svc.cluster.local:9092
    - name: mirrord.temporary_group_id
      value: "true"
```

Splits then patch the workload's consumer-group environment variables (the ones under `appConfig.groupId`) to a generated temporary group, alongside the topic rewrite. The operator keeps the original group to itself, so it never negotiates a protocol with the application's client - any client library works. Offsets are preserved: the operator keeps committing into the original group, and the workload resumes exactly where it left off when the split ends.

The operator can only join the original group once every pod of the previous generation has left it, so the split waits for the workload's rollout to finish. By default it waits 180 seconds and then fails the session. If your rollout takes longer (many replicas, a long termination grace period, a consumer that stays in the group until its session timeout expires), raise the wait with the `mirrord.group_join_timeout` property, in seconds:

```yaml
    - name: mirrord.temporary_group_id
      value: "true"
    - name: mirrord.group_join_timeout
      value: "600"
```

Temporary group names follow the temporary topic name format (`mirrord-tmp-...`), so if you use group ACLs, the application's credentials must be allowed to join groups with that prefix, and the operator's credentials need `DeleteGroups` for cleanup.

{% hint style="info" %}
`mirrord.temporary_group_id` requires mirrord operator `3.195.0` or later, and `mirrord.group_join_timeout` requires operator `3.204.0` or later - earlier operators reject it as an unknown `mirrord.` key.
{% endhint %}

## Setting a filter

For the full filter reference (`queue_type`, `message_filter`, `jq_filter`, `payload_protobuf`), see the [overview](../queue-splitting.md#setting-a-filter-for-a-mirrord-run). Kafka uses `queue_type: Kafka` and supports `message_filter` on Kafka headers, `jq_filter` on a JSON representation of the whole record, and `payload_protobuf` for decoding protobuf record values before the jq program runs.

### Filtering on headers

```json
{
  "operator": true,
  "target": "deployment/meme-app/container/consumer",
  "feature": {
    "split_queues": {
      "views-topic": {
        "queue_type": "Kafka",
        "message_filter": {
          "baggage": ".*mirrord-session=alice.*"
        }
      }
    }
  }
}
```

In the example above, the local application will receive a subset of messages from the Kafka queue with ID `views-topic`. All received messages will have a Kafka header `baggage` containing `mirrord-session=alice`.

### Filtering on message content with jq

`jq_filter` runs a jq program on a JSON document the operator builds for each record:

* `topic` - the topic name.
* `partition` - the partition number.
* `offset` - the record offset.
* `timestamp` - the record timestamp in milliseconds (present only when the record carries one).
* `key` - the record key (absent when the record has no key).
* `payload` - the record value (absent when the record has no value).
* `headers` - an object mapping header names to their values (always present, possibly empty; a repeated header name keeps the last value).

`key`, `payload`, and header values are UTF-8 strings, or base64-encoded when not valid UTF-8. A record matches if the jq program outputs `true`.

This lets you route messages by fields inside the message body. For example, to receive only messages whose JSON payload has a `merchantId` of `2137` under `data`:

```json
{
  "operator": true,
  "target": "deployment/meme-app/container/consumer",
  "feature": {
    "split_queues": {
      "views-topic": {
        "queue_type": "Kafka",
        "jq_filter": ".payload | fromjson | .data.merchantId == 2137"
      }
    }
  }
}
```

If both `message_filter` and `jq_filter` are specified for the same queue, both must match for a message to reach the local application. Records for which the jq program errors (for example, a non-JSON payload piped to `fromjson`) are treated as not matching and stay on the deployed application's path.

{% hint style="warning" %}
`jq_filter` for Kafka requires mirrord operator `3.183.0` or later and mirrord CLI `3.232.0` or later, and is only supported with the default `librdkafka` client. Sessions using the Java client (`mirrord.client_implementation: java`, required for Kafka Streams) fail with a clear error when a `jq_filter` is set.
{% endhint %}

### Filtering on protobuf payloads

Some topics carry raw protobuf bytes instead of JSON - for example CDC events serialized as plain protobuf, with no JSON envelope and no schema registry prefix. `payload_protobuf` decodes each record's value with your schema before the jq program runs, and exposes the decoded message as an extra `payload_decoded` field:

```json
{
  "operator": true,
  "target": "deployment/meme-app/container/consumer",
  "feature": {
    "split_queues": {
      "cdc-topic": {
        "queue_type": "Kafka",
        "payload_protobuf": {
          "schema_file": "schemas/cdc_record.proto",
          "message_type": "com.example.cdc.Record"
        },
        "jq_filter": ".payload_decoded.merchant_id == 2137 and .payload_decoded.metadata.transactionType == \"PURCHASE\""
      }
    }
  }
}
```

* `schema_file` - path to the `.proto` file defining the value's message type. The mirrord CLI compiles it locally, resolving imports against the file's own directory (add `include_directories` for extra import roots), so the operator never needs access to your schema files. Users with pre-compiled schemas can set `descriptor_base64` (a base64-encoded `FileDescriptorSet`, as produced by `protoc --descriptor_set_out --include_imports`) instead.
* `message_type` - the fully-qualified name of the value's message type.

In `payload_decoded`, field names appear exactly as written in the schema, enum values as their names, and 64-bit integers as JSON numbers. Fields at their default value are included. Records whose value fails to decode with the given schema are treated as not matching and stay on the deployed application's path.

{% hint style="warning" %}
`payload_protobuf` only supports plain protobuf values - schema registry framing (magic byte and schema id prefix) is not supported. Like `jq_filter`, it requires the default `librdkafka` client.
{% endhint %}

## FAQ

### How do I authenticate the operator's Kafka client with an SSL certificate?

Set the PEM contents as Kafka client properties on the `MirrordPropertyList`:

```yaml
apiVersion: mirrord.metalbear.co/v1
kind: MirrordPropertyList
metadata:
  name: kafka-connection
  namespace: meme
spec:
  properties:
    - name: bootstrap.servers
      value: kafka.default.svc.cluster.local:9093
    - name: security.protocol
      value: SSL
    # Contents of the PEM file with client certificate.
    - name: ssl.certificate.pem
      value: "..."
    # Contents of the PEM file with client private key.
    - name: ssl.key.pem
      value: "..."
    # Contents of the PEM file with CA.
    - name: ssl.ca.pem
      value: "..."
    # Password for the client private key (if password protected).
    - name: ssl.key.password
      value: "..."
```

To avoid putting secrets directly in the resource, store them in a Kubernetes [`Secret`](https://kubernetes.io/docs/concepts/configuration/secret/) and reference them with `valueFrom`:

```yaml
apiVersion: mirrord.metalbear.co/v1
kind: MirrordPropertyList
metadata:
  name: kafka-connection
  namespace: meme
spec:
  properties:
    - name: bootstrap.servers
      value: kafka.default.svc.cluster.local:9093
    - name: security.protocol
      value: SSL
    - name: ssl.certificate.pem
      valueFrom:
        secretKeyRef:
          name: mirrord-kafka-ssl
          key: ssl.certificate.pem
    - name: ssl.key.pem
      valueFrom:
        secretKeyRef:
          name: mirrord-kafka-ssl
          key: ssl.key.pem
    - name: ssl.ca.pem
      valueFrom:
        secretKeyRef:
          name: mirrord-kafka-ssl
          key: ssl.ca.pem
```

{% hint style="info" %}
By default, the mirrord operator has read access only to the secrets in the operator's namespace. The `MirrordPropertyList` itself lives in the target's namespace.
{% endhint %}

### How do I authenticate the operator's Kafka client with a Java KeyStore?

{% hint style="info" %}
Reading credentials from Java KeyStores requires mirrord operator `3.199.0` or later. On earlier versions, unpack the stores into PEM files yourself (see [Unpacking a Java KeyStore by hand](kafka.md#unpacking-a-java-keystore-by-hand)).
{% endhint %}

Put the same `ssl.*` properties your JVM application already uses on the `MirrordPropertyList`. The operator loads the stores, unpacks them, and hands the certificates and the private key to its Kafka client as PEM, so you do not have to run `keytool`/`openssl` or keep a second copy of the credentials just for Kafka splitting.

The store itself can be given in two ways:

* **Inline**, base64-encoded, in the `mirrord.ssl.truststore.base64` / `mirrord.ssl.keystore.base64` properties. Recommended: nothing has to be mounted into the operator pod.
* **As a file**, in the standard `ssl.truststore.location` / `ssl.keystore.location` properties. The path is read from the **operator pod's** filesystem, so the store has to be mounted there - not into the target workload.

Base64-encode the stores and put them in a `Secret`, next to their passwords:

```sh
kubectl create secret generic kafka-stores --namespace meme \
  --from-literal=truststore.jks.base64="$(base64 < truststore.jks | tr -d '\n')" \
  --from-literal=keystore.jks.base64="$(base64 < keystore.jks | tr -d '\n')" \
  --from-literal=truststore.password=changeit \
  --from-literal=keystore.password=changeit
```

{% hint style="warning" %}
The `Secret` must hold the **base64 text** of the store, not the raw store bytes. Property values are read as UTF-8 strings, so a key created with `--from-file=keystore.jks` cannot be read.
{% endhint %}

Then reference the secret keys from the `MirrordPropertyList`:

```yaml
apiVersion: mirrord.metalbear.co/v1
kind: MirrordPropertyList
metadata:
  name: kafka-connection
  namespace: meme
spec:
  properties:
    - name: bootstrap.servers
      value: kafka.default.svc.cluster.local:9093
    - name: security.protocol
      value: SSL
    # Truststore: the CAs the broker's certificate is verified against.
    - name: mirrord.ssl.truststore.base64
      valueFrom:
        secretKeyRef:
          name: kafka-stores
          key: truststore.jks.base64
    - name: ssl.truststore.password
      valueFrom:
        secretKeyRef:
          name: kafka-stores
          key: truststore.password
    # Keystore: the client certificate and private key, for mutual TLS.
    - name: mirrord.ssl.keystore.base64
      valueFrom:
        secretKeyRef:
          name: kafka-stores
          key: keystore.jks.base64
    - name: ssl.keystore.password
      valueFrom:
        secretKeyRef:
          name: kafka-stores
          key: keystore.password
```

If the broker does not require client certificates, set only the truststore properties.

{% hint style="info" %}
The `Secret` is resolved in the namespace the `MirrordPropertyList` was found in, and by default the operator can only read secrets in its own namespace. Either grant it read access to secrets in the target's namespace, or keep both the `Secret` and the `MirrordPropertyList` next to the operator - see [Sharing Property Lists Across Namespaces](../queue-splitting.md#sharing-property-lists-across-namespaces).
{% endhint %}

The properties the operator understands:

| Property                                | Meaning                                                                                                                                        |
| --------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| `ssl.truststore.location`               | Path of the truststore, read from the operator pod's filesystem.                                                                                |
| `mirrord.ssl.truststore.base64`         | Base64-encoded truststore, given inline instead of a file.                                                                                      |
| `ssl.truststore.password`               | Optional. A truststore holds only public certificates, so it is unpacked without one; the password only verifies the store's integrity digest.   |
| `ssl.keystore.location`                 | Path of the keystore, read from the operator pod's filesystem.                                                                                  |
| `mirrord.ssl.keystore.base64`           | Base64-encoded keystore, given inline instead of a file.                                                                                        |
| `ssl.keystore.password`                 | Required for a JKS keystore, which is encrypted.                                                                                                |
| `ssl.key.password`                      | Password of the private key inside the keystore. Defaults to the keystore password, like the JVM client.                                         |
| `mirrord.ssl.keystore.alias`            | Which key entry to authenticate with. Required only when the keystore holds more than one private key.                                           |
| `ssl.endpoint.identification.algorithm` | Passed through. The JVM's empty value (hostname verification off) is translated for the operator's client.                                       |

A few things to keep in mind:

* `ssl.truststore.type` and `ssl.keystore.type` are ignored - the format is detected from the store's contents. `keytool` writes PKCS#12 by default since JDK 9, so a store named `.jks` is routinely PKCS#12 and the declared type disagrees with the file. JKS, JCEKS, PKCS#12 and PEM stores are all recognized.
* A **PKCS#12 keystore** is passed to the Kafka client untouched, since it reads that format natively. It therefore has to be given as a file with `ssl.keystore.location`; an inline base64 PKCS#12 keystore is rejected.
* A **PKCS#12 truststore** is not supported. Convert it to JKS with `keytool -importkeystore -deststoretype JKS`, or extract the certificates and set `ssl.ca.pem` as described in [authenticating with an SSL certificate](kafka.md#how-do-i-authenticate-the-operators-kafka-client-with-an-ssl-certificate).
* Credentials given as PEM in the Kafka properties (`ssl.truststore.certificates`, `ssl.keystore.certificate.chain`, `ssl.keystore.key`, i.e. `ssl.keystore.type=PEM`) are accepted too, and mapped to the client's `ssl.ca.pem`, `ssl.certificate.pem` and `ssl.key.pem`.
* This conversion belongs to the default `librdkafka` client backend. With `mirrord.client_implementation: java`, the JVM sidecar reads the stores itself, so use `ssl.truststore.location` / `ssl.keystore.location` with the stores mounted into the operator pod. Every `mirrord.ssl.*` property is rejected for that backend - not only the inline `mirrord.ssl.truststore.base64` / `mirrord.ssl.keystore.base64`, but `mirrord.ssl.keystore.alias` as well, since the JVM client has no way to pick a key entry. A keystore holding more than one private key has to be narrowed to a single entry first, with `keytool -importkeystore -srcalias <alias>`.

If a store fails to load, the mirrord session that tried to use it fails with the reason - wrong password, ambiguous key alias, unreadable path, and so on.

### Unpacking a Java KeyStore by hand

On operators older than `3.199.0`, extract the certificates and the key into PEM files first:

```sh
# Convert keystore.jks to PKCS12 format.
keytool -importkeystore \
  -srckeystore keystore.jks \
  -srcstoretype JKS \
  -destkeystore keystore.p12 \
  -deststoretype PKCS12

# Extract client certificate PEM from the converted keystore
openssl pkcs12 -in keystore.p12 -clcerts -nokeys -out client-cert.pem

# Extract client private key PEM from the converted keystore.
openssl pkcs12 -in keystore.p12 -nocerts -nodes -out client-key.pem

# Convert truststore.jks to PKCS12 format.
keytool -importkeystore \
  -srckeystore truststore.jks \
  -srcstoretype JKS \
  -destkeystore truststore.p12 \
  -deststoretype PKCS12

# Extract CA PEM from the converted truststore.
openssl pkcs12 -in truststore.p12 -nokeys -out ca-cert.pem
```

Then, follow the guide for [authenticating with an SSL certificate](kafka.md#how-do-i-authenticate-the-operators-kafka-client-with-an-ssl-certificate).
