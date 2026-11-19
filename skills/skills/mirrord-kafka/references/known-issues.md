# Known Issues & Gotchas — Kafka Splitting

These are real issues encountered by customers and tracked internally.

> **CRD note:** the current resources are `MirrordSplitConfig` + `MirrordPropertyList`. `MirrordKafkaTopicsConsumer` + `MirrordKafkaClientConfig` are deprecated (still supported). Issues below apply to both models unless marked *(legacy)*.

## Consumer identity is required per queue
**New model:** each `MirrordSplitConfig` queue must set **exactly one** of `appConfig.groupId` (standard consumers) or `appConfig.appId` (Kafka Streams). Omitting both is a config error.

*(legacy)* **groupIdSources is required (INT-366):** in `MirrordKafkaTopicsConsumer`, the schema marks `groupIdSources` optional but the operator returns HTTP 500 when it's omitted. Always include it.

## Content/body filtering IS supported via jq_filter
Earlier the operator could only filter Kafka messages by header (`message_filter`). This is no longer a limitation: **`jq_filter`** runs a jq program over a JSON representation of the record (`topic`, `partition`, `offset`, `timestamp`, `key`, `payload`, `headers`) and matches when it outputs `true` — so you can route by fields inside the message body. Requires operator **3.183.0+**, CLI **3.232.0+**, and the default **librdkafka** client (not the Java/Streams client). If both `message_filter` and `jq_filter` are set, both must match.

## min.insync.replicas not copied to ephemeral topics (INT-384)
**Status:** Open bug  
When the source topic has `replication.factor=1` and `min.insync.replicas=1`, the operator copies the replication factor but not `min.insync.replicas` to ephemeral topics. Those topics inherit the broker default (often `2`), so with `acks=all` the producer waits for 2 replicas when only 1 exists → timeout.  
**Workaround:** Add `acks: "1"` to the `MirrordPropertyList` (or legacy `MirrordKafkaClientConfig`) properties if using single-replica topics.

## Java KeyStore (JKS) not directly supported (INT-165)
**Status:** Resolved (operator 3.199.0+)  
The operator now loads Java KeyStores (JKS, JCEKS, PKCS#12) natively: put the same `ssl.*` properties the JVM app already uses on the `MirrordPropertyList`, plus `mirrord.ssl.truststore.base64`/`mirrord.ssl.keystore.base64` for an inline base64-encoded store (or `ssl.truststore.location`/`ssl.keystore.location` for a store mounted into the **operator pod**). See `references/mirrord-property-list-crd.md`.  
**Workaround (older operators):** Convert JKS to PEM:
```sh
# Keystore → PKCS12 → PEM
keytool -importkeystore -srckeystore keystore.jks -srcstoretype JKS \
  -destkeystore keystore.p12 -deststoretype PKCS12
openssl pkcs12 -in keystore.p12 -clcerts -nokeys -out client-cert.pem
openssl pkcs12 -in keystore.p12 -nocerts -nodes -out client-key.pem

# Truststore → PKCS12 → PEM
keytool -importkeystore -srckeystore truststore.jks -srcstoretype JKS \
  -destkeystore truststore.p12 -deststoretype PKCS12
openssl pkcs12 -in truststore.p12 -nokeys -out ca-cert.pem
```
Then use `ssl.certificate.pem`, `ssl.key.pem`, `ssl.ca.pem` in the client config. This path (and PKCS#12 truststores, which native loading doesn't support) still applies regardless of operator version.

## Vault-injected config not supported (PRO-102)
**Status:** Resolved (operator 3.201.0+)  
Kafka splitting used to only work when topic name and group ID were exposed as environment variables in the pod spec (either directly or via ConfigMap). HashiCorp Vault `vault-agent-injector` and secrets-store CSI drivers inject config at runtime into a file, which is invisible as an env var.  
**Fix:** set a `podFile` source on the `appConfig.topic`/`groupId`/`appId` entry, naming the absolute in-container path (`podFile.path`) and, if needed, the container to read it from (`podFile.container` — defaults to a `vault-agent` sidecar, else the pod's first app container). The operator reads the file via `pods/exec` (`cat`) in a running target pod, then mounts a Secret with the substituted content over the same path, so the app never has to change how it reads the file.  
**Workaround (older operators):** Expose the topic name and group ID as regular env vars in the pod template for the operator to read. The actual application can still use Vault for other config.

## Operational friction with many Kafka clusters (SOL-144)
**Status:** Improved by the new model  
The deprecated `MirrordKafkaClientConfig` was scoped to the operator namespace, so many clusters/namespaces got cumbersome. The current `MirrordPropertyList` lives in the **target's namespace** alongside the `MirrordSplitConfig`.  
**Guidance:** Share one `MirrordPropertyList` across a `MirrordSplitConfig`'s queues via `spec.clientConfigs.kafka`. *(legacy)* `MirrordKafkaClientConfig` supported `parent` inheritance for shared properties; `MirrordPropertyList` has no `parent` — repeat shared properties or share by name.

## Kafka Streams needs the Java client (INT-226)
**Status:** Supported  
The operator's default librdkafka consumer is incompatible with Kafka Streams' custom partition assignment; a JVM-based Kafka proxy handles it.  
**How to enable:** set `mirrord.client_implementation: java` on the `MirrordPropertyList`, use `appConfig.appId` (not `groupId`) on the queue, and enable `operator.kafkaSplittingSidecar.enabled: true` in the Helm chart. `jq_filter` is **not** available with the Java client.

## Non-Streams clients with custom group protocols (e.g. KafkaJS) — `INCONSISTENT_GROUP_PROTOCOL`
**Status:** Resolved (operator 3.195.0+)  
Some client libraries that are *not* Kafka Streams — KafkaJS is the common case — advertise their own partition-assignment protocol name, which the operator's librdkafka consumer can't join a group with, producing an `INCONSISTENT_GROUP_PROTOCOL` error.  
**Fix:** set `mirrord.temporary_group_id: "true"` on the `MirrordPropertyList`. The operator then patches the workload's consumer-group env vars to a generated temporary group instead of joining the original group, so any client library works; offsets are preserved. This is distinct from the Kafka Streams fix above (which is for actual Streams apps using `appConfig.appId`).

## Managed Kafka rejects temporary topics with `PolicyViolation`
**Status:** Resolved (operator 3.191.0+)  
Some managed Kafka platforms enforce a minimum replication factor for new topics — Confluent Cloud requires 3 — and reject the operator's default factor-1 temporary topics with a `PolicyViolation` broker error.  
**Fix:** set `mirrord.split_topic.replication_factor` on the `MirrordPropertyList` — a positive number, `copy` (match the source topic's factor, recommended for Confluent Cloud), or `-1` (broker default).

## Ephemeral topic cleanup errors (INT-392)
**Status:** Open — Urgent  
Operator logs may show `UnknownTopicOrPartition` errors when trying to delete temporary topics that were already cleaned up. Can lead to split timeouts.  
**If seen:** Check operator logs, restart the operator if needed. This is a known race condition being worked on.

## Strimzi-specific setup (INT-258)
**Status:** Open — Needs documentation  
Strimzi clusters use custom K8s resources for topic/user management. The operator needs permissions to manage temporary topics, and targets need permissions to read them.  
**Guidance:** For Strimzi setups, ensure the operator's Kafka user has ACLs to create/delete topics with the `mirrord-tmp-*` prefix, and that target workload users can read from those topics.

## Customizing temporary topic names
Available since chart `1.27` / operator `3.114.0`.  
Default format: `mirrord-tmp-{{RANDOM}}{{FALLBACK}}{{ORIGINAL_TOPIC}}`  
Configurable via `operator.kafkaSplittingTopicFormat` Helm value or `OPERATOR_KAFKA_SPLITTING_TOPIC_FORMAT` env var.  
All three template variables (`{{RANDOM}}`, `{{FALLBACK}}`, `{{ORIGINAL_TOPIC}}`) are required.

---

## Quick Symptom Lookup

| Symptom | Likely cause | Reference |
|---------|-------------|-----------|
| Operator returns 500 when starting session | *(legacy)* `groupIdSources` missing from `MirrordKafkaTopicsConsumer`; *(new)* neither `appConfig.groupId` nor `appConfig.appId` set on the queue | INT-366 |
| Body/payload filter needed but "only headers supported" | Outdated — use `jq_filter` (librdkafka only) | — |
| Session timeout + `UnknownTopicOrPartition` in logs | Ephemeral topic cleanup race condition | INT-392 |
| Producer timeout with single-replica topics | `min.insync.replicas` not copied to ephemeral topics | INT-384 |
| `InconsistentGroupProtocol` error, Kafka Streams app | Kafka Streams incompatibility (needs JVM proxy) | INT-226 |
| `InconsistentGroupProtocol` error, non-Streams client (e.g. KafkaJS) | Custom partition-assignment protocol — set `mirrord.temporary_group_id: "true"` | — |
| Splitting doesn't start, env vars not found | Vault/CSI-injected config — use a `podFile` source instead (operator 3.201.0+) | PRO-102 |
| Long rollout causes `mirrord.temporary_group_id` splits to time out at 180s | Raise `mirrord.group_join_timeout` (operator 3.204.0+) | — |
| Auth fails with JKS credentials | Operator 3.199.0+ loads JKS/JCEKS/PKCS#12 natively via `mirrord.ssl.*`; older operators need PEM conversion | INT-165 |
| Splitting works but permissions fail on temp topics | Strimzi ACLs need `mirrord-tmp-*` prefix rules | INT-258 |
| Splitting fails with `PolicyViolation` broker error (e.g. Confluent Cloud) | Managed platform enforces a minimum replication factor — set `mirrord.split_topic.replication_factor` | — |

## Operator version requirements

Some features require a minimum version:
- **≥ 3.114.0**: JVM-based Kafka proxy (Kafka Streams support), custom temp topic naming
- **operator ≥ 3.170.0 / CLI ≥ 3.221.0**: `MirrordSplitConfig` + `MirrordPropertyList` (the current CRDs). On older operators, only the legacy CRDs exist.
- **operator ≥ 3.183.0 / CLI ≥ 3.232.0**: `jq_filter` for Kafka (librdkafka client only).
- **operator ≥ 3.191.0**: `mirrord.split_topic.replication_factor` (control the replication factor of temporary/split topics).
- **operator ≥ 3.195.0**: `mirrord.temporary_group_id` (fixes `INCONSISTENT_GROUP_PROTOCOL` for non-Streams clients like KafkaJS).
- **operator ≥ 3.198.0**: `appConfig.topic`/`groupId`/`appId` `volume` source (read the name from a file mounted from a ConfigMap volume instead of an env var).
- **operator ≥ 3.199.0**: native Java KeyStore credentials (`mirrord.ssl.*` properties) — see the JKS entry above.
- **operator ≥ 3.201.0**: `appConfig.topic`/`groupId`/`appId` `podFile` source (read the name from a file that exists only inside the running pods, e.g. Vault- or CSI-injected).
- **operator ≥ 3.204.0**: `mirrord.group_join_timeout` (raise the 180s default wait for a temporary-group split to join, for slow rollouts).
- `payload_protobuf` (decode raw protobuf record values for `jq_filter`, librdkafka client only) — no minimum version is called out separately in the docs beyond the librdkafka requirement; treat it as needing a recent operator/CLI and confirm against the changelog if a session rejects the field.
- Always check the operator version when troubleshooting: `kubectl get deploy mirrord-operator -n mirrord -o jsonpath='{.spec.template.spec.containers[0].image}'`
