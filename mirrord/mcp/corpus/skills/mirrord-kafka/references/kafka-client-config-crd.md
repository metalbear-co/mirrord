# MirrordKafkaClientConfig CRD Reference (DEPRECATED)

> **⚠️ Deprecated.** `MirrordKafkaClientConfig` is replaced by **`MirrordPropertyList`** (`references/mirrord-property-list-crd.md`). Existing resources still work, and `MirrordSplitConfig`'s `clientConfig` falls back to a `MirrordKafkaClientConfig` of the same name in the operator namespace. Use this file only when helping maintain or migrate a legacy setup; generate `MirrordPropertyList` for anything new.

## Overview
Configures the mirrord Operator's Kafka client for queue splitting. Must be created in the **operator's namespace** (typically `mirrord`).

**API Version:** `queues.mirrord.metalbear.co/v1alpha`  
**Kind:** `MirrordKafkaClientConfig`

## spec fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `properties` | list of `{name, value}` | Yes | Kafka client properties passed as a `.properties` file. Full list: [librdkafka CONFIGURATION.md](https://github.com/confluentinc/librdkafka/blob/master/CONFIGURATION.md) |
| `parent` | string | No | Name of another `MirrordKafkaClientConfig` in the same namespace to inherit from. Child properties override parent; setting `value: null` removes an inherited property. |
| `loadFromSecret` | string | No | Reference to a K8s Secret in `<namespace>/<name>` format. Each key-value in the secret becomes a property. |
| `authenticationExtra` | object | No | Custom auth methods that go beyond client properties. |

### properties[] entry

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | Yes | Kafka client property name (e.g. `bootstrap.servers`) |
| `value` | string or null | Yes | Property value. Use `null` only when inheriting from parent to remove a property. |

### authenticationExtra

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `kind` | string | Yes | Authentication type. Currently supported: `MSK_IAM` |
| `awsRegion` | string | Yes (for MSK_IAM) | AWS region for MSK IAM auth |

When `kind: MSK_IAM`, two properties are auto-merged:
- `sasl.mechanism=OAUTHBEARER`
- `security.protocol=SASL_SSL`

## Property resolution priority (highest to lowest)
1. Child `spec.properties`
2. Child `spec.loadFromSecret`
3. Parent `spec.properties`
4. Parent `spec.loadFromSecret`

## Important constraints
- **Namespace:** Must always be in the operator's namespace (default: `mirrord`)
- **group.id:** Always overwritten by the operator at runtime — do not set it
- **Secret access:** By default the operator only has read access to secrets in the operator's namespace

## Common properties

| Property | Typical value | Notes |
|----------|---------------|-------|
| `bootstrap.servers` | `kafka.default.svc.cluster.local:9092` | Required for any setup |
| `client.id` | `mirrord-operator` | Optional identifier |
| `security.protocol` | `SASL_SSL`, `SSL`, `PLAINTEXT` | Depends on cluster auth |
| `sasl.mechanism` | `PLAIN`, `SCRAM-SHA-256`, `SCRAM-SHA-512`, `OAUTHBEARER` | For SASL auth |
| `sasl.username` / `sasl.password` | credentials | For SASL PLAIN/SCRAM |
| `ssl.certificate.pem` | PEM content | Client cert for mTLS |
| `ssl.key.pem` | PEM content | Client private key |
| `ssl.ca.pem` | PEM content | CA cert to verify broker |
| `ssl.key.password` | string | If private key is password-protected |

## Authentication patterns

### Plain/no auth (dev clusters)
```yaml
spec:
  properties:
  - name: bootstrap.servers
    value: kafka.default.svc.cluster.local:9092
```

### SASL PLAIN

> **Security:** Never inline `sasl.password` (or any credential) as a literal value in generated YAML. Reference a Kubernetes Secret in the operator's namespace via `loadFromSecret`. The user creates the Secret themselves; the agent must not handle the password value.

```yaml
spec:
  # Secret in the operator namespace with keys: sasl.username, sasl.password
  loadFromSecret: mirrord/kafka-sasl-creds
  properties:
  - name: bootstrap.servers
    value: kafka-broker:9093
  - name: security.protocol
    value: SASL_SSL
  - name: sasl.mechanism
    value: PLAIN
```

The user creates the backing Secret from files, not literals (so credentials don't appear in shell history):
```sh
# User saves username/password to files, then:
kubectl create secret generic kafka-sasl-creds \
  --from-file=sasl.username=/path/to/username-file \
  --from-file=sasl.password=/path/to/password-file \
  -n mirrord
# User: delete the temporary files after the Secret is created.
```

### SSL/mTLS (via secret)
```yaml
spec:
  loadFromSecret: mirrord/kafka-ssl-creds
  properties:
  - name: bootstrap.servers
    value: kafka-broker:9093
  - name: security.protocol
    value: SSL
```

### MSK IAM (AWS)
```yaml
spec:
  authenticationExtra:
    kind: MSK_IAM
    awsRegion: us-east-1
  properties:
  - name: bootstrap.servers
    value: b-1.mycluster.kafka.us-east-1.amazonaws.com:9098
```

### Inheritance example
```yaml
# Parent
apiVersion: queues.mirrord.metalbear.co/v1alpha
kind: MirrordKafkaClientConfig
metadata:
  name: base-config
  namespace: mirrord
spec:
  properties:
  - name: bootstrap.servers
    value: kafka.default.svc.cluster.local:9092
  - name: message.send.max.retries
    value: "4"
---
# Child (inherits + overrides)
apiVersion: queues.mirrord.metalbear.co/v1alpha
kind: MirrordKafkaClientConfig
metadata:
  name: with-auth
  namespace: mirrord
spec:
  parent: base-config
  properties:
  - name: security.protocol
    value: SASL_SSL
  - name: message.send.max.retries
    value: null  # removes inherited property
```
