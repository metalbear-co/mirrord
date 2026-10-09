# MirrordSplitConfig — Temporal reference

The `MirrordSplitConfig` links a target worker workload to the Temporal task queues it polls, so the operator knows what to split. The CRD type is defined in the cluster when the operator is installed with `operator.temporalSplitting: true` — verify with `kubectl get crd mirrordsplitconfigs.queues.mirrord.metalbear.co`.

- **API version:** `queues.mirrord.metalbear.co/v1`
- **Kind:** `MirrordSplitConfig`
- **Namespace:** must be the **same namespace as the target workload**.
- **Versions:** operator **3.170.0+**, CLI **3.221.0+**.

## Spec fields

### `spec.targetRef` (required)

Reference to the workload whose pods poll the task queue:

| Field | Description |
| --- | --- |
| `apiVersion` | API version of the workload, e.g. `apps/v1` |
| `kind` | `Deployment`, `StatefulSet`, or `Rollout` |
| `name` | Name of the workload |

### `spec.clientConfigs` (optional)

Sets a default `MirrordPropertyList` per queue kind, so individual queues don't need to repeat `clientConfig`:

```yaml
clientConfigs:
  temporal: temporal-config
```

### `spec.queues[]` (required)

One entry per Temporal task queue the worker consumes:

| Field | Required | Description |
| --- | :---: | --- |
| `id` | ✓ | Arbitrary queue ID that developers reference from mirrord.json `split_queues`. |
| `kind` | ✓ | Must be `temporal`. |
| `clientConfig` | No | Name of the `MirrordPropertyList` with the Temporal connection. Falls back to `spec.clientConfigs.temporal`. |
| `appConfig.taskQueue` | ✓ | How the worker discovers the task queue name. The operator patches this to a virtual task queue name. |
| `appConfig.temporalAddress` | No | Env var holding the Temporal frontend address. The operator patches it so the worker connects to the operator's proxy instead of the real frontend. |
| `appConfig.temporalNamespace` | No | Env var holding the Temporal namespace. |
| `queueConfig` | No | Name of a `MirrordPropertyList` with per-queue settings (`max_buffered_tasks` — see the property-list reference). |

### `appConfig` source structure

Each `appConfig` field is a **list of sources**, same structure as other queue services:

| Key | Description |
| --- | --- |
| `env` | Exact env var name holding the value. |
| `envLike` | Regex over env var names, for when the exact name varies. |
| `volume` | Read the value from a file mounted from a `configMap` volume (`volume.name` for the pod-spec volume, `volume.file` for the path within it), instead of an environment variable. Requires operator **3.198.0+**. `env`/`envLike` takes precedence if also set on the same entry; `fallback` and `containers` don't apply to it — the file is shared by every container that mounts the volume. The operator serves your local worker a copy of the file carrying its own session queue names, in-flight over the session; the original ConfigMap is never modified. |
| `podFile` | Read the value from a file that exists only inside the running pods, with no ConfigMap/Secret behind it (e.g. rendered by `vault-agent-injector` or a secrets-store CSI driver). `podFile.path` is the absolute in-container path; `podFile.container` is the container to read it from, defaulting to a `vault-agent` sidecar if present, else the pod's first application container. Requires operator **3.201.0+**. The other source wins if `env`/`envLike`/`volume` is also set on the same entry, and `podFile` is ignored; `fallback` doesn't apply to it. The operator reads the file via `pods/exec` (`cat`) in a running target pod, then mounts a Secret with the substituted content over the same path — the same kind of restart env var injection causes. |
| `fallback` | Literal value used when no env var matches. |
| `valueSelector` | A selector over nested keys and `.[]` (to iterate arrays or object values) for JSON-valued env vars — not a full jq expression; pipes and functions are not supported. |
| `valuePattern` | Regex to swap a name embedded inside a larger value. |
| `containers` | Container names the source applies to (omit for all containers). For `podFile`, this limits which containers get the overriding mount; without one, every application container gets it. |

The operator can only read env vars that are either defined directly in the pod template (`value`, or `valueFrom` via ConfigMap reference) or loaded from ConfigMaps via `envFrom`. Env vars injected at runtime (e.g. by Vault sidecars) are invisible to it this way — use a `podFile` source instead.

### `spec.drainTimeout` (optional)

After the last session against a target ends, the operator keeps the split's temporary resources alive for the drain timeout so a new session can reuse them, then tears them down. It does **not** wait for in-flight work to finish first.

| `drainTimeout` (seconds) | Behavior |
| --- | --- |
| unset | Tear down as soon as the last session ends (same as `0`). |
| `0` | Tear down immediately. In-flight work may be lost. |
| `N` | Keep resources for up to `N` seconds, then tear down. |

A `spec.drainTimeout` on the `MirrordSplitConfig` wins over the cluster-wide default.

## Full example

```yaml
apiVersion: queues.mirrord.metalbear.co/v1
kind: MirrordSplitConfig
metadata:
  name: temporal-worker-split
  namespace: workflows
spec:
  targetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: temporal-worker
  clientConfigs:
    temporal: temporal-config
  queues:
    - id: orders-task-queue
      kind: temporal
      appConfig:
        taskQueue:
          - env: TEMPORAL_TASK_QUEUE
        temporalAddress:
          - env: TEMPORAL_ADDRESS
        temporalNamespace:
          - env: TEMPORAL_NAMESPACE
```

This says: target the deployment `temporal-worker` in `workflows`; the Temporal connection comes from the `temporal-config` property list; the worker reads its task queue name from `TEMPORAL_TASK_QUEUE`; the operator patches `TEMPORAL_ADDRESS` to point the worker at its proxy and reads the Temporal namespace from `TEMPORAL_NAMESPACE`; developers reference this queue as `orders-task-queue` in mirrord.json.

## Filter semantics (developer side)

mirrord.json entries for these queues use `queue_type: "Temporal"`.

- `message_filter` keys: `workflow_id`, `workflow_type`, `activity_type`, `header.<name>`, or any other key (matched against a Temporal search attribute of that name). All entries must match. Empty `message_filter` with no `jq_filter` = match-none.
- `jq_filter` runs over a per-task JSON doc with `task_type` (`"activity"` or `"workflow"`):
  - activity tasks: `workflow_namespace`, `workflow_id`, `run_id`, `workflow_type`, `activity_type`, `activity_id`, `attempt`, `header`, `input` (array of decoded payloads)
  - workflow tasks: `workflow_id`, `run_id`, `workflow_type`, `attempt`, `task_queue`, `cron_schedule`, `identity`, `first_execution_run_id`, `header`, `search_attributes`, `memo`, `input`
- Both filters set = both must match.
- `queue_mode: "mirror"` is **not supported** for Temporal (steal-only).
- If two sessions' filters match the same task, the most recently started session gets it.
- With `operator.injectSessionKeyHeader` enabled, only **activity** tasks routed to a session carry the `mirrord-key` header; workflow tasks are never stamped (their header lives in replayed workflow history).

## Observing splits

With operator + CLI **3.223.0+**, splitting status is live-queryable:

```bash
mirrord queues status            # one row per active split (alias: mirrord qs status)
mirrord qs status -A             # all namespaces
mirrord qs status <name>         # detail: filters, resolved queues, patched pods
kubectl get queuesplits -A       # same data via a read-only Kubernetes-style API
```
