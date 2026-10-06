# mirrord Operator — Helm values reference

Curated digest of the important knobs in the operator chart. This file is **hand-maintained** — for the exact current defaults and any value not listed here, read the verbatim upstream snapshot in `references/values.yaml` (kept in sync automatically by the `update-references` workflow), or the live source:
https://raw.githubusercontent.com/metalbear-co/charts/main/mirrord-operator/values.yaml

> When the workflow opens a PR because `values.yaml` changed, review whether this digest needs a matching edit.

> **Secrets rule:** never inline a cloud API key, license key, or PEM in `values.yaml` or on the
> CLI. Use a Kubernetes Secret ref or a Google Secret Manager ref (see [Authentication](#authentication)).

## Top-level

| Value | Default | Notes |
|-------|---------|-------|
| `namespace` | `mirrord` | Namespace the operator runs in. |
| `createNamespace` | `true` | Set `false` to manage the namespace yourself. |
| `roleNamespaces` | `[]` | Namespaces to create a namespaced mirrord role in (empty = none). Bind users to it yourself. |
| `extraObjects` | `[]` | Extra Kubernetes resources rendered as templates with the chart. |
| `openshift` | `false` | Set `true` on OpenShift to render a SecurityContextConstraints. |

## Authentication

### Cloud API key (default) — `cloud.apiKey.*`

Exactly one of:

| Value | Notes |
|-------|-------|
| `cloud.apiKey.keyRef` | Name of a Kubernetes Secret holding the key under data key `apiKey`. **Recommended.** |
| `cloud.apiKey.gsmRef` | Google Secret Manager ID `projects/PROJECT_ID/secrets/SECRET_NAME/versions/latest` (read via ADC; see `sa.gcpSa`). |
| `cloud.apiKey.key` | Inline value — plaintext in the pod spec. Dev/test only. |

Generate the key in the dashboard (**Settings**, [app.metalbear.com](https://app.metalbear.com)); shown once. Rotate/revoke from the dashboard.

When generating the key, an org admin also chooses **identity sharing** (ticked by default): with it on, the usage dashboard shows developer usernames and session targets instead of hashes; with it off, usage metrics stay anonymized. Set `cloud.anonymizeData: true` to keep metrics anonymized regardless of the key's setting — this also covers legacy license keys and self-hosted License Server auth, which never send identity. A change takes effect at the operator's next cloud token refresh, no restart needed.

### License — `license.*` (deprecated for cloud auth; used for license server / offline)

| Value | Notes |
|-------|-------|
| `license.key` | Inline license key. Deprecated for cloud; still the shared secret for a self-hosted license server. |
| `license.keyRef` | Secret name holding the key (data key `OPERATOR_LICENSE_KEY`). |
| `license.keyGsmRef` | GSM ID for the license key. |
| `license.file.secret` | Secret name for the license PEM (default `mirrord-operator-license`). |
| `license.file.secret.data.license.pem` | Inline PEM as a YAML literal block (air-gapped). |
| `license.pemRef` | Secret name holding a `license.pem` (air-gapped). |
| `license.pemGsmRef` | GSM ID for the license PEM. |
| `license.licenseServer` | URL of a self-hosted license server (air-gapped). |
| `license.allowSeatOverages` | `true` — allow usage above max seats (Enterprise). Set `false` to enforce. |

## Feature flags (all under `operator.*`, default `false` unless noted)

**Queue splitting:** `sqsSplitting`, `kafkaSplitting`, `rmqSplitting`, `gcpPubsubSplitting`, `azureServiceBusSplitting`, `redisPubsubSplitting`, `temporalSplitting`, `bullmqSplitting`.
- `kafkaSplittingSidecar.enabled` (default `false`) — JVM sidecar for Kafka Streams clients; `image`, `port` (33000), `requests`/`limits`.
- `temporalProxy.port` (7233), `idleKafkaSplitTtlMillis`, `kafkaSplittingTopicFormat`, `sqsSplittingLingerTimeout` (60000), `queueSplittingWaitForReadyTarget` (`true`).

**DB branching (per engine):** `mysqlBranching`, `pgBranching`, `mariadbBranching`, `mongodbBranching`, `mssqlBranching`, `redisBranching`, `dynamodbBranching`, `clickhouseBranching`, `cockroachdbBranching`, `spannerBranching`, `genericBranching`.
- `dbBranchingLiteralCredentials` (default `true`) — cluster-wide Secret perms so users can pass literal DB credentials in mirrord config instead of referencing existing Secrets.
- Per-engine pod overrides: `operator.<engine>BranchConfig.dbPod.*` — `image.registry`, `initImage.registry`, `migrationImages.flyway.registry`, `migrationImages.liquibase.registry`, `imagePullSecrets`, `imagePullPolicy`, `labels`, `annotations`, `volume`, `resources`. Generic: `genericBranchConfig.dbPod.allowedImages` (glob list; absent = all allowed).
- **Storage** (operator `3.194.0`+): each branch gets its own PVCs by default — 20Gi data + 20Gi dump, on the cluster's default StorageClass, sized cluster-wide by `dbBranching.databasePvcSize`/`initPvcSize` and per engine by `<engine>BranchConfig.dbPod.storage.{dataSize,initSize,storageClassName}`. `dbPod.storage.kind: "emptyDir"` opts an engine back into node-local storage (capped by the older `dbBranching.databasePodVolumeLimit`/`initPodVolumeLimit`), which is also the automatic fallback on clusters with no default StorageClass. Default branch pod memory limit is `2Gi` (raised from `512Mi`); tune via `dbPod.resources`. See the `mirrord-db-branching` skill for the full config shape.

**Preview environments:** `previewEnv` (default `false`).
- `shareIngress.shareDomain` — domain share hosts are minted under (`<slug>.<shareDomain>`, no leading `*.`); empty = no link sharing.
- `shareIngress.stableSlugs` (default `false`) — mint share hosts without the random suffix, as `<sanitized key>.<shareDomain>`, so a link can be built from the session key alone (e.g. by a PR bot) before the preview starts. Guessable — only enable when the ingress in front of `mirrord-share-ingress` authenticates every request. One live session per host: a second session whose key sanitizes to the same host fails at creation. See the `mirrord-prev-env` skill.
- `preview.cleanupAfterMins` (15), `preview.annotations`, `preview.labels`, `preview.idleHoldBufferMessages` (512), `preview.idleHoldBufferBytes` (8388608), `preview.imagePullPolicy`.

**Other operator toggles:** `metrics` (Prometheus), `applicationPauseAutoSync` (ArgoCD), `suspendFluxControllers` (Flux), `isolatePodsRestart` (`true`), `injectSessionKeyHeader` (`true`), `disableTelemetries` (Enterprise only), `requireFrontProxyClientCert` (`true`).

## Operator deployment — `operator.*`

| Value | Default | Notes |
|-------|---------|-------|
| `image` | `ghcr.io/metalbear-co/operator` | Override for internal registry. |
| `replicas` | `1` | Leader + standby replicas for HA. |
| `port` | `443` | Change to `3000`/`8443` if 443 is restricted; ensure node reachability. Binds `[::]` with `0.0.0.0` fallback. |
| `requests` / `limits` | `100m/100Mi` · `500m/200Mi` | ~200 concurrent sessions at defaults; raise for more. |
| `clusterName` | unset | Friendly cluster label in telemetry / license server. |
| `logLevel` | `mirrord=info,operator=info,kube_runtime=warn` | stdout log filter. |
| `jsonLog` | `false` | JSON stdout logs. |
| `otelLogExportUrl` / `otelTraceExportUrl` / `otelLogLevel` | unset | OTel export (independent of stdout). As of operator `3.186.0`, both URLs may reference vars set in `operator.extraEnv` via `$(VAR_NAME)` syntax. |
| `sessionSetupDeadlineSeconds` | `180` | How long a session may wait for its first client connection (the "starting up" phase — e.g. while queue splitting waits for target pods to roll) before it's closed. Requires operator/chart `3.191.0`+. |
| `sessionUnusedTtlSeconds` | `30` | How long a session may sit idle after its client disconnects (the "connected" phase) before it's closed. Raise for clients on slow/unreliable networks. Requires operator/chart `3.191.0`+. |
| `maxSessionTimeSeconds` | unset | Cap on total session lifetime, spanning both phases above. The only value that can close a session someone is actively using. |
| `communicationTimeoutMillis` | `60000` | How long either the client or the agent may go silent on an already-connected session before it's closed — the operator times each side separately and closes as soon as **either** exceeds this. Applies regardless of chart version; requires operator Helm chart `3.210.0`+ to actually **change** the value (earlier charts always apply the 60000 default). Symptom of hitting it: session drops with `Layer Communication Timeout` in the operator log. |
| `noPodTargetsSessionTimeoutMillis` | `60000` | Timeout when no ready target pods. |
| `subscribeEventBufferSize` | `2048` | `mirrord subscribe` event buffer depth. |
| `copyTarget.useAgentImage` | `true` | Copy-target dummy container uses the agent image (has `sleep`). |
| `jiraWebhookUrl` | unset | mirrord for Jira integration. |
| `extraEnv`, `extraVolumes`, `extraVolumeMounts`, `podAnnotations`, `podLabels`, `labels`, `tolerations`, `affinity`, `nodeSelector`, `imagePullSecrets` | — | Standard deployment knobs. As of operator `3.186.0`, each `extraEnv` entry can be a plain string or a full env-var spec (`valueFrom` with `fieldRef`/`secretKeyRef`/`configMapKeyRef`) — e.g. a downward API value like `status.hostIP` — so `otelLogExportUrl`/`otelTraceExportUrl` can be built from cluster-local values: `HOST_IP` via `fieldRef: status.hostIP` + `OTLP_PORT: "4318"` → `otelLogExportUrl: "http://$(HOST_IP):$(OTLP_PORT)/v1/logs"`. |

### Multi-cluster — `operator.multiCluster.*`

Primary-cluster orchestration via Envoy. `enabled` (primary only), `defaultCluster` (required when enabled — where stateful ops happen), `managementOnly` (`true`), `sessionSetupDeadlineSeconds` (180) and `sessionTtlSeconds` (60) for the multi-cluster session's setup/connected phases (the older `remoteSessionTimeoutSecs`/`sessionTtlSecs` spellings are still honored), `clusters` map. Members set `operator.multiClusterMember: true` (+ `multiClusterMemberIamGroup` for EKS IAM or `multiClusterMemberAzureGroup` for AKS Workload Identity). Per-cluster `authType`: `eks` (IAM, auto-refresh), `aks` (Workload Identity), `bearerToken` (auto-refresh), `mtls` (manual rotation). Remote-cluster credentials live in Secrets labeled `operator.metalbear.co/remote-cluster-credentials=true`.

Routing: incoming traffic and queue splitting go to **all** Workload clusters (each cluster's operator filters/handles locally); env vars, file ops, DNS, outgoing traffic, and DB branching go to the **Default** cluster only. Queue splitting works for every supported queue service in multi-cluster (SQS, Kafka, RabbitMQ, GCP Pub/Sub, Azure Service Bus, Redis Pub/Sub, Temporal, BullMQ) — each Workload cluster still needs its own queue-service credentials (e.g. AWS creds with SQS permissions), independent of the multi-cluster `authType`. **Limitations:** targetless mode isn't supported (a target is required); a session only becomes multi-cluster when connecting to the Primary (connecting directly to a downstream cluster starts a regular single-cluster session there); the per-target RBAC check doesn't apply — restrict per-target access with `MirrordPolicy`/`MirrordClusterPolicy` instead.

**Preview environment replicas** (operator/chart `3.193.0`+ on every cluster, mirrord `3.247.0`+): `operator.multiCluster.preview.mode` — `"default-cluster"` (default: preview pods run only on the Default cluster) or `"replicas"` (a preview pod runs on every Workload cluster, so HTTP traffic is served wherever it enters; queue traffic already reaches every cluster in either mode). Set the same mode on **every** operator (Primary and all Workload clusters). All replicas share one branch database, reached through an operator-to-operator tunnel initiated by the Primary — Workload clusters need no direct route to the Default cluster. For branching previews, upgrade the Primary and Workload operators together (mismatched versions refuse the branch tunnel) and allow egress from workload namespaces to the operator's namespace on port `4980`. `operator.multiCluster.preview.maxTunnelStreams` (default `256`) caps concurrent open database connections per Workload cluster across all its preview replicas. See the `mirrord-prev-env` skill for the preview-environment side.

## Agent — `agent.*`

| Value | Default | Notes |
|-------|---------|-------|
| `image` / `image.registry` / `image.tag` | `ghcr.io/metalbear-co/mirrord` | Override for internal registry. |
| `tls` | `false` | TLS-secure agent connections; requires agent ≥ 3.97.0. |
| `injectHeaders` | `true` | Agent marks whether a request went through the agent / was stolen. |
| `port` | random high port | Set a fixed port for service-mesh exclusions. |
| `extraEnv` | `{}` | Extra agent env vars. |
| `priorityClass.*` | create `mirrord-agent-pod`, value `1000000000` | PriorityClass for agent pods. |
| `extraConfig` | — | Arbitrary agent config not covered above (`json_log`, `labels`, `annotations`, `tolerations`, `privileged`, …). |

## Service accounts & TLS

- `sa.create` (`true`), `sa.name` (`mirrord-operator`), `sa.roleArn` (AWS IRSA), `sa.gcpSa` (GCP impersonation), `sa.azureClientId` (AKS Workload Identity).
- `service.name` (`mirrord-operator`).
- `tls.secret` (`mirrord-operator-tls`), `tls.useExistingSecret` (`false`), `tls.apiService.insecureSkipTLSVerify` (`true`; set `false` with a verified cert), `tls.certManager.enabled` (`false`, `createIssuer`, `issuer`, `certificate`), `tls.data.tls.crt`/`tls.key`.

## RBAC roles created by the chart

- Namespaced: `role.mirrord-operator-user` (per `roleNamespaces`).
- Cluster: `clusterRole.mirrord-operator-user-basic`, `clusterRole.mirrord-operator-user`, `clusterRole.mirrord-operator-ci` (scoped for CI / preview environments). Each accepts `labels` (for aggregated RBAC). Bind these to your users/groups/service accounts yourself.
