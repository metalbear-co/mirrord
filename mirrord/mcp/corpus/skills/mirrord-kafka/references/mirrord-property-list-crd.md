# MirrordPropertyList CRD Reference (Kafka client connection)

The **current** resource for the operator's Kafka client connection (replaces the deprecated `MirrordKafkaClientConfig`). A `MirrordSplitConfig` queue references it by name via `clientConfig`.

**API Version:** `mirrord.metalbear.co/v1`
**Kind:** `MirrordPropertyList`
**Namespace:** looked up in two places, in order: (1) the **target workload's namespace** (also the namespace of its `MirrordSplitConfig`) — the recommended default, and it wins if a list of the same name also exists in the operator's namespace; (2) the **operator's own namespace**, as a fallback that lets one connection config be shared across many teams/namespaces (requires operator **3.191.0+**; earlier operators only look in the target's namespace). ConfigMap/Secret refs inside a property list resolve in whichever namespace the list itself was found in. A property list in the target's namespace that the operator can't parse fails the session outright — it does not fall through to the operator's namespace. This lookup is unrelated to the deprecated `MirrordKafkaClientConfig`, which only ever lived in the operator's namespace.

## spec

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `properties` | list of `{name, value}` or `{name, valueFrom}` | Yes | Kafka client properties, passed straight to the underlying client. A few `mirrord.`-prefixed keys are consumed by the operator (see below). |

Each property is either:
- `- name: <key>` / `value: "<string>"` — inline value, or
- `- name: <key>` / `valueFrom: { secretKeyRef: { name: <secret>, key: <key> } }` — read from a Kubernetes Secret.

Full list of Kafka client properties: [librdkafka CONFIGURATION.md](https://github.com/confluentinc/librdkafka/blob/master/CONFIGURATION.md).

## Operator-specific (`mirrord.`-prefixed) keys

Consumed by the operator, not forwarded to the Kafka client:

| Key | Values | Description |
|-----|--------|-------------|
| `mirrord.client_implementation` | `librdkafka` (default), `java` | Kafka client backend. Use `java` for **Kafka Streams** consumers (`appConfig.appId`). |
| `mirrord.auth.kind` | `MSK_IAM` | Extra authentication mechanism (only supported value). See [MSK IAM](#aws-msk-iam). |
| `mirrord.auth.aws_region` | e.g. `eu-south-1` | AWS region; required when `mirrord.auth.kind` is `MSK_IAM`. |
| `mirrord.temporary_group_id` | `"true"` | Works around `INCONSISTENT_GROUP_PROTOCOL` errors from clients that advertise their own partition-assignment protocol (e.g. KafkaJS). Requires operator **3.195.0+**. See [KafkaJS / custom group protocols](#kafkajs--custom-group-protocols-mirrordtemporary_group_id). |
| `mirrord.group_join_timeout` | seconds, e.g. `"600"` | Raises the default 180s wait for a `mirrord.temporary_group_id` split to join the original consumer group once the previous generation's pods have left it. Requires operator **3.204.0+**. |
| `mirrord.split_topic.replication_factor` | a positive number, `copy`, or `-1` | Replication factor for temporary (split) topics. Default is `1`. Requires operator **3.191.0+**. See [Temporary Topic Replication Factor](#temporary-topic-replication-factor). |
| `mirrord.ssl.truststore.base64` / `mirrord.ssl.keystore.base64` | base64 text of the store | Inline Java KeyStore (JKS/JCEKS/PKCS#12), given instead of a file. Requires operator **3.199.0+**. See [Java KeyStore credentials](#java-keystore-credentials). |
| `mirrord.ssl.keystore.alias` | key entry name | Which private key to use from a keystore holding more than one. Requires operator **3.199.0+**. |

> **Do not set `group.id`.** The consumer group used by the operator's own client is managed by mirrord.

## Namespace lookup order and legacy fallback

The operator resolves a `clientConfig` name in this order:

1. A `MirrordPropertyList` in the target's namespace.
2. A `MirrordPropertyList` in the operator's namespace (operator **3.191.0+**) — for sharing one connection config across namespaces.
3. A legacy `MirrordKafkaClientConfig` of the same name in the operator's namespace, if no `MirrordPropertyList` was found in either namespace above.

This keeps older setups working while you migrate.

## Common properties

| Property | Typical value | Notes |
|----------|---------------|-------|
| `bootstrap.servers` | `kafka.default.svc.cluster.local:9092` | Required. |
| `security.protocol` | `PLAINTEXT`, `SSL`, `SASL_SSL`, `SASL_PLAINTEXT` | Depends on cluster auth. |
| `sasl.mechanism` | `PLAIN`, `SCRAM-SHA-256`, `SCRAM-SHA-512`, `OAUTHBEARER` | For SASL. |
| `sasl.username` / `sasl.password` | credentials | For SASL PLAIN/SCRAM — use `valueFrom.secretKeyRef`. |
| `ssl.certificate.pem` / `ssl.key.pem` / `ssl.ca.pem` | PEM contents | For SSL/mTLS. |
| `ssl.key.password` | string | If the private key is password-protected. |

## Examples

### Plaintext (dev)

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

### AWS MSK IAM

```yaml
spec:
  properties:
    - name: bootstrap.servers
      value: b-1.mycluster.kafka.eu-south-1.amazonaws.com:9098
    - name: mirrord.auth.kind
      value: MSK_IAM
    - name: mirrord.auth.aws_region
      value: eu-south-1
```

When `mirrord.auth.kind: MSK_IAM`, the operator automatically adds `sasl.mechanism=OAUTHBEARER` and `security.protocol=SASL_SSL`. Tokens are produced via the default AWS credential provider chain — the easiest path is IAM role assumption, annotating the operator's service account with the role ARN via `sa.roleArn` in the operator Helm chart.

### SSL/mTLS via Kubernetes Secret (recommended for credentials)

Never inline PEM key material or passwords. Store them in a Secret and reference with `valueFrom.secretKeyRef`:

```yaml
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

> By default the operator has read access only to secrets in the operator's namespace, even though the `MirrordPropertyList` itself lives in the target's namespace. Confirm the referenced Secret is reachable by the operator; if not, the admin must grant read access.

### Kafka Streams (Java client)

```yaml
spec:
  properties:
    - name: bootstrap.servers
      value: kafka.default.svc.cluster.local:9092
    - name: mirrord.client_implementation
      value: java
```

Kafka Streams also requires the operator's Kafka sidecar (`operator.kafkaSplittingSidecar.enabled: true` in the Helm chart).

### KafkaJS / custom group protocols (`mirrord.temporary_group_id`)

Some client libraries — KafkaJS, for example — advertise their own partition-assignment protocol name, which the operator's librdkafka-based consumer cannot join a group with. This surfaces as an `INCONSISTENT_GROUP_PROTOCOL` error when a split session starts.

Set `mirrord.temporary_group_id: "true"` on the Kafka `MirrordPropertyList`:

```yaml
spec:
  properties:
    - name: bootstrap.servers
      value: kafka.default.svc.cluster.local:9092
    - name: mirrord.temporary_group_id
      value: "true"
```

With this set, splits patch the workload's consumer-group env vars (`appConfig.groupId`) to a generated temporary group, alongside the topic rewrite. The operator keeps the original group to itself, so it never negotiates a protocol with the application's client — any client library works. Offsets are preserved: the operator keeps committing into the original group, and the workload resumes exactly where it left off when the split ends.

The operator can only join the original group once every pod of the previous generation has left it, so the split waits for the workload's rollout to finish — 180 seconds by default, then the session fails. For a slower rollout (many replicas, a long termination grace period, a consumer that stays in the group until its session timeout expires), raise the wait with `mirrord.group_join_timeout`, in seconds:

```yaml
spec:
  properties:
    - name: mirrord.temporary_group_id
      value: "true"
    - name: mirrord.group_join_timeout
      value: "600"
```

Requires operator **3.204.0+**; earlier operators reject it as an unknown `mirrord.` key.

### Temporary Topic Replication Factor (`mirrord.split_topic.replication_factor`)

By default, the operator creates temporary (split) topics with a replication factor of 1. Some managed Kafka platforms enforce a minimum and reject these topics — for example, Confluent Cloud requires a factor of 3, so splitting sessions fail with a `PolicyViolation` broker error.

Set `mirrord.split_topic.replication_factor` on the Kafka `MirrordPropertyList` to control the factor. Requires operator **3.191.0+**; earlier operators reject it as an unknown `mirrord.` key.

```yaml
spec:
  properties:
    - name: bootstrap.servers
      value: kafka.default.svc.cluster.local:9092
    - name: mirrord.split_topic.replication_factor
      value: copy
```

Accepted values:
- a positive number — used as-is for every temporary topic.
- `copy` — copy the replication factor derived from the original topic. Recommended when temporary topics must match source topics on managed platforms like Confluent Cloud.
- `-1` — use the broker's default replication factor.

Temporary group names follow the temporary topic name format (`mirrord-tmp-...`) — if you use group ACLs, the application's credentials must be allowed to join groups with that prefix, and the operator's credentials need `DeleteGroups` for cleanup. Requires operator **3.195.0+**.

## Java KeyStore credentials

Requires operator **3.199.0+**. On older operators, unpack the stores into PEM files yourself — see [Unpacking a Java KeyStore by hand](#unpacking-a-java-keystore-by-hand) below.

Put the same `ssl.*` properties the JVM application already uses on the `MirrordPropertyList`. The operator loads the stores, unpacks them, and hands the certificates and private key to its Kafka client as PEM — no `keytool`/`openssl` step, and no second copy of the credentials just for Kafka splitting.

The store can be given two ways:
- **Inline**, base64-encoded, via `mirrord.ssl.truststore.base64` / `mirrord.ssl.keystore.base64`. Recommended — nothing to mount into the operator pod.
- **As a file**, via the standard `ssl.truststore.location` / `ssl.keystore.location`. The path is read from the **operator pod's** filesystem (not the target workload).

Base64-encode the stores into a Secret next to their passwords:

```sh
kubectl create secret generic kafka-stores --namespace meme \
  --from-literal=truststore.jks.base64="$(base64 < truststore.jks | tr -d '\n')" \
  --from-literal=keystore.jks.base64="$(base64 < keystore.jks | tr -d '\n')" \
  --from-literal=truststore.password=changeit \
  --from-literal=keystore.password=changeit
```

> The Secret must hold the **base64 text** of the store, not the raw bytes — property values are read as UTF-8 strings, so a key created with `--from-file=keystore.jks` cannot be read.

Reference the secret keys from the `MirrordPropertyList`:

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
        secretKeyRef: { name: kafka-stores, key: truststore.jks.base64 }
    - name: ssl.truststore.password
      valueFrom:
        secretKeyRef: { name: kafka-stores, key: truststore.password }
    # Keystore: the client certificate and private key, for mutual TLS.
    - name: mirrord.ssl.keystore.base64
      valueFrom:
        secretKeyRef: { name: kafka-stores, key: keystore.jks.base64 }
    - name: ssl.keystore.password
      valueFrom:
        secretKeyRef: { name: kafka-stores, key: keystore.password }
```

If the broker doesn't require client certificates, set only the truststore properties.

Properties the operator understands:

| Property | Meaning |
|----------|---------|
| `ssl.truststore.location` | Path of the truststore, read from the **operator pod's** filesystem. |
| `mirrord.ssl.truststore.base64` | Base64-encoded truststore, given inline instead of a file. |
| `ssl.truststore.password` | Optional — a truststore holds only public certificates; the password only verifies the store's integrity digest. |
| `ssl.keystore.location` | Path of the keystore, read from the **operator pod's** filesystem. |
| `mirrord.ssl.keystore.base64` | Base64-encoded keystore, given inline instead of a file. |
| `ssl.keystore.password` | Required for a JKS keystore (encrypted). |
| `ssl.key.password` | Password of the private key inside the keystore. Defaults to the keystore password. |
| `mirrord.ssl.keystore.alias` | Which key entry to authenticate with. Required only when the keystore holds more than one private key. |
| `ssl.endpoint.identification.algorithm` | Passed through; the JVM's empty value (hostname verification off) is translated for the operator's client. |

Things to know:
- `ssl.truststore.type` / `ssl.keystore.type` are ignored — the format is detected from the store's contents (`keytool` writes PKCS#12 by default since JDK 9, so a `.jks`-named file is routinely PKCS#12). JKS, JCEKS, PKCS#12, and PEM stores are all recognized.
- A **PKCS#12 keystore** is passed to the Kafka client untouched, so it must be given as a file via `ssl.keystore.location` — an inline base64 PKCS#12 keystore is rejected.
- A **PKCS#12 truststore** is not supported — convert it to JKS (`keytool -importkeystore -deststoretype JKS`), or extract the certificates into `ssl.ca.pem`.
- PEM credentials given directly (`ssl.truststore.certificates`, `ssl.keystore.certificate.chain`, `ssl.keystore.key`, i.e. `ssl.keystore.type=PEM`) are accepted too, mapped to `ssl.ca.pem` / `ssl.certificate.pem` / `ssl.key.pem`.
- This conversion belongs to the default `librdkafka` backend. With `mirrord.client_implementation: java`, the JVM sidecar reads the stores itself: use `ssl.truststore.location` / `ssl.keystore.location` with the stores mounted into the operator pod, and expect every `mirrord.ssl.*` property (including `mirrord.ssl.keystore.alias`) to be rejected — narrow a multi-key keystore with `keytool -importkeystore -srcalias <alias>` first.

If a store fails to load, the session that tried to use it fails with the reason (wrong password, ambiguous key alias, unreadable path, etc.).

### Unpacking a Java KeyStore by hand

On operators older than `3.199.0`, the client only supports PEM. Convert a Java KeyStore first:

```sh
keytool -importkeystore -srckeystore keystore.jks -srcstoretype JKS \
  -destkeystore keystore.p12 -deststoretype PKCS12
openssl pkcs12 -in keystore.p12 -clcerts -nokeys -out client-cert.pem
openssl pkcs12 -in keystore.p12 -nocerts -nodes -out client-key.pem

keytool -importkeystore -srckeystore truststore.jks -srcstoretype JKS \
  -destkeystore truststore.p12 -deststoretype PKCS12
openssl pkcs12 -in truststore.p12 -nokeys -out ca-cert.pem
```

Then use `ssl.certificate.pem`, `ssl.key.pem`, `ssl.ca.pem` (via a Secret).

## Mapping from the deprecated resource

`MirrordKafkaClientConfig` properties map one-to-one onto `MirrordPropertyList` properties. Differences:
- **Namespace:** `MirrordPropertyList` lives in the **target's** namespace; `MirrordKafkaClientConfig` lived in the **operator's** namespace.
- **MSK IAM:** the old `authenticationExtra: { kind: MSK_IAM, awsRegion: ... }` becomes the `mirrord.auth.kind` / `mirrord.auth.aws_region` properties.
- **Secrets:** the old `loadFromSecret` becomes per-property `valueFrom.secretKeyRef`.
- The old `parent` inheritance has no direct equivalent — repeat shared properties, or use `spec.clientConfigs.kafka` on the `MirrordSplitConfig` to share one `MirrordPropertyList` across queues.
