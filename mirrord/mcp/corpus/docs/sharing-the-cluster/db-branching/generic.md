---
title: Generic
description: Branch any stateful service with mirrord by supplying your own container image
tags:
  - alpha
  - team
  - enterprise
---

This page covers generic DB branching - for databases, caches, and other stateful services that mirrord has no built-in engine for (InfluxDB, Valkey, an internal service, and so on). For the general concepts, the full list of config fields, and how a session behaves, see the [DB Branching overview](../db-branching.md).

A generic branch runs the container image you supply and starts **empty** by default: there are no built-in copy modes, no IAM authentication, and a single redirected port. It is the fallback that unblocks you when no first-class engine exists - engines with built-in support offer copy modes, schema handling, and better errors, so prefer them when available. When an empty branch isn't useful, you can supply a [copy Job](#copying-data-into-the-branch) - your own image that fills the branch with schema and data before the session starts.

{% hint style="info" %}
Generic branching requires operator `3.183.0`, mirrord CLI `3.232.0`, and operator Helm chart `3.183.0` with the `operator.genericBranching` value set to `true`. The [copy Job](#copying-data-into-the-branch) and [profile-supplied branch defaults](#admin-profiles-branch-defaults-and-copy) need a newer operator - a config using them against an older one fails immediately with a clear "operator does not support" error.

If the operator doesn't support generic branching (older version, or the Helm value is off), the session fails immediately with a clear "operator does not support generic db branching" error rather than timing out.
{% endhint %}

## How It Works

Because mirrord doesn't know the engine, you declare the connection parameters your service needs under `connection.params` - the fixed slots (`host`, `port`, `user`, `password`, `database`) plus **any custom key** (for example `token`, `org`, `vhost`). The operator resolves each declared parameter from the target pod and injects it into the branch container as an env var named `MIRRORD_PARAM_<NAME>` (a Secret-backed parameter arrives as a `secretKeyRef` - the operator never reads its value).

You reference these in `command`, `args`, and `env` using Kubernetes' native `$(VAR)` syntax, so the branch bootstraps itself with the **same values the app already uses**. mirrord then only rewrites the app's `host`/`port` vars to point at the branch - the app's credential vars stay untouched, and they're valid against the branch because it was bootstrapped with those same values.

Concretely, if the target pod has:

```
DATABASE_URL=postgresql://root@cockroach-main:26257/defaultdb?sslmode=disable
```

the operator creates the branch pod from your config, and the **local process** sees:

```
DATABASE_URL=postgresql://root@10.244.0.42:26257/defaultdb?sslmode=disable
```

Only the fragments captured by your `value_pattern` (here the host) are replaced with the branch pod's address - the scheme, user, database, and query params are untouched, so the app parses the URL exactly as before but now talks to the branch. The override exists only inside the mirrord session: the source database, the target pod, and every other user of it are unaffected.

Two built-in variables are always available alongside the parameters: `MIRRORD_BRANCH_ID`, and `MIRRORD_DATABASE_NAME` when the shared `name` field is set. Use `$$(...)` for a literal `$(...)`.

## Basic Configuration

| Field | Description |
| --- | --- |
| `image` | *(Required unless a `profile` supplies it)* Full image reference for the branch container, including the tag. The shared `version` field is not allowed - the tag lives here. Admins can restrict which images are accepted via `genericBranchConfig.dbPod.allowedImages` - see [Restricting Branch Images](../db-branching.md#restricting-branch-images). |
| `port` | *(Required unless a `profile` supplies it)* The port the branched service listens on. Used for the default readiness probe and as the port the app is redirected to. |
| `command` / `args` | *(Optional)* Entrypoint override for the branch container. Values may reference `$(MIRRORD_PARAM_<NAME>)`. |
| `env` | *(Optional)* Extra environment variables for the branch container, with the same `$(VAR)` references. Keys must not start with `MIRRORD_PARAM_`. |
| `readiness` | *(Optional)* Readiness check for the branch container. Defaults to a TCP probe on `port`. See [Readiness](#readiness). |
| `copy` | *(Optional)* A one-shot Job that copies schema and data into the branch before it turns Ready. See [Copying Data into the Branch](#copying-data-into-the-branch). |
| `profile` | *(Optional)* Name of an admin-defined profile in the operator's generic branch config. A profile can supply the branch defaults (`image`, `port`, `command`, `args`, `env`, `readiness`) and a `copy` Job, so your mirrord.json shrinks to `type`/`id`/`profile`/`connection`. See [Admin Profiles](#admin-profiles-branch-defaults-and-copy). |

How you feed the params into the branch depends entirely on how *your image* bootstraps itself - some images take startup flags (use `args`), others read well-known env vars on first boot (use `env`), and some need nothing at all. The examples below cover one of each style, from the most minimal to the most involved.

## Configuration Examples

<details>

<summary>CockroachDB - the minimal config (no credentials)</summary>

Insecure single-node dev mode needs no bootstrap at all: just the image, the SQL port, the startup args, and host/port patterns. The app reads a postgres-style URL - note the host pattern anchors on the `@` that ends the userinfo part, and the rewrite leaves the scheme, user, database, and query params intact.

```json
{
  "feature": {
    "db_branches": [
      {
        "type": "generic",
        "id": "my-cockroach-branch",
        "ttl_secs": 600,
        "image": "cockroachdb/cockroach:v24.1.5",
        "port": 26257,
        "connection": {
          "params": {
            "host": { "env_var_name": "COCKROACH_URL", "value_pattern": "@(?P<host>[^:/]+)" },
            "port": { "env_var_name": "COCKROACH_URL", "value_pattern": ":(?P<port>[0-9]+)/" }
          }
        },
        "args": ["start-single-node", "--insecure"],
        "readiness": { "type": "http_get", "path": "/health?ready=1", "port": 8080 }
      }
    ]
  }
}
```

Target pod env: `COCKROACH_URL=postgresql://root@cockroach-main:26257/defaultdb?sslmode=disable` - rewritten locally exactly as shown in [How It Works](#how-it-works).

Also demonstrates: an `http_get` readiness probe on a **different port** than the redirected one - health is checked on CockroachDB's admin API (8080) while the app is redirected to the SQL port (26257).

</details>

<details>

<summary>Aerospike - minimal config, binary wire protocol</summary>

The image ships a default in-memory `test` namespace, so like CockroachDB there is nothing to bootstrap. The app talks Aerospike's native binary protocol - generic branching is protocol-agnostic; only the env var rewrite matters:

```json
{
  "feature": {
    "db_branches": [
      {
        "type": "generic",
        "id": "my-aerospike-branch",
        "ttl_secs": 600,
        "image": "aerospike/aerospike-server:latest",
        "port": 3000,
        "connection": {
          "params": {
            "host": { "env_var_name": "AEROSPIKE_ADDR", "value_pattern": "^(?P<host>[^:]+):" },
            "port": { "env_var_name": "AEROSPIKE_ADDR", "value_pattern": ":(?P<port>[0-9]+)$" }
          }
        }
      }
    ]
  }
}
```

No `command`, `args`, `env`, or `readiness` at all - the image's defaults plus the default TCP probe on `port` are enough.

</details>

<details>

<summary>Valkey - bootstrap via <code>args</code></summary>

The app reads a composite `VALKEY_ADDR=valkey-main:6379` env var and a `VALKEY_PASSWORD` that comes from a Kubernetes Secret. Valkey (like Redis) is configured through command-line flags, so the branch passes the app's real password to `--requirepass`:

```json
{
  "feature": {
    "db_branches": [
      {
        "type": "generic",
        "id": "my-valkey-branch",
        "ttl_secs": 600,
        "image": "valkey/valkey:8-alpine",
        "port": 6379,
        "connection": {
          "params": {
            "host": { "env_var_name": "VALKEY_ADDR", "value_pattern": "^(?P<host>[^:]+):" },
            "port": { "env_var_name": "VALKEY_ADDR", "value_pattern": ":(?P<port>[0-9]+)$" },
            "password": "VALKEY_PASSWORD"
          }
        },
        "args": ["valkey-server", "--requirepass", "$(MIRRORD_PARAM_PASSWORD)"]
      }
    ]
  }
}
```

When the session starts:

1. The operator resolves `VALKEY_PASSWORD` from the target pod - since the pod reads it from a Secret, it arrives on the branch container as a `secretKeyRef` under the name `MIRRORD_PARAM_PASSWORD`. The kubelet expands `$(MIRRORD_PARAM_PASSWORD)` in the args when the container starts, so the branch requires the **same password the app already uses** - and the secret value never appears in the pod spec.
2. The default TCP probe on `6379` passes and the branch turns Ready.
3. Locally, only the host and port *fragments* of `VALKEY_ADDR` are rewritten in place (the `value_pattern` captures), so the app still sees the `host:port` shape it expects - now pointing at the branch. `VALKEY_PASSWORD` is untouched and just works.

</details>

<details>

<summary>InfluxDB - bootstrap via <code>env</code> (first-boot setup mode)</summary>

The app reads `INFLUXDB_URL` plus token/org/bucket vars. InfluxDB's image isn't configured through flags - it bootstraps through its own **first-boot setup mechanism**: when the official `influxdb` image starts with `DOCKER_INFLUXDB_INIT_MODE=setup`, its entrypoint creates the initial admin user, organization, bucket, and API token from the other `DOCKER_INFLUXDB_INIT_*` env vars before serving traffic.

{% hint style="info" %}
The `DOCKER_INFLUXDB_INIT_*` names are **InfluxDB's** convention, not mirrord's - they're documented on the [influxdb Docker image](https://hub.docker.com/_/influxdb). mirrord's job is only to fill them: the three `$(MIRRORD_PARAM_*)` references carry the app's real token/org/bucket into the setup, while `MODE`/`USERNAME`/`PASSWORD` are plain literals (a throwaway admin login the app never uses). Check your own image's docs for its equivalent bootstrap knobs.
{% endhint %}

```json
{
  "feature": {
    "db_branches": [
      {
        "type": "generic",
        "id": "my-influx-branch",
        "ttl_secs": 600,
        "image": "docker.io/library/influxdb:2.7",
        "port": 8086,
        "connection": {
          "params": {
            "host": { "env_var_name": "INFLUXDB_URL", "value_pattern": "https?://(?P<host>[^:/]+)" },
            "port": { "env_var_name": "INFLUXDB_URL", "value_pattern": ":(?P<port>[0-9]+)" },
            "token": { "secret": "influx-creds", "key": "token" },
            "org": "INFLUXDB_ORG",
            "bucket": "INFLUXDB_BUCKET"
          }
        },
        "env": {
          "DOCKER_INFLUXDB_INIT_MODE": "setup",
          "DOCKER_INFLUXDB_INIT_USERNAME": "admin",
          "DOCKER_INFLUXDB_INIT_PASSWORD": "mirrord-branch",
          "DOCKER_INFLUXDB_INIT_ADMIN_TOKEN": "$(MIRRORD_PARAM_TOKEN)",
          "DOCKER_INFLUXDB_INIT_ORG": "$(MIRRORD_PARAM_ORG)",
          "DOCKER_INFLUXDB_INIT_BUCKET": "$(MIRRORD_PARAM_BUCKET)"
        },
        "readiness": { "type": "http_get", "path": "/health" }
      }
    ]
  }
}
```

When the session starts:

1. The operator creates (or reuses, by `id`) a branch pod running `influxdb:2.7`, with the setup env filled from the params resolved off the target pod: `token` straight from the `influx-creds` Kubernetes Secret (as a `secretKeyRef`), `org` and `bucket` from the pod's env vars. The image's setup mode then creates that same org, bucket, and admin token on first start.
2. The `http_get` probe on `/health` passes once setup finished and the server is up - only then does the branch turn Ready.
3. Locally, the host/port fragments of `INFLUXDB_URL` are rewritten to point at the branch (the URL shape and scheme survive). `INFLUXDB_TOKEN`, `INFLUXDB_ORG`, and `INFLUXDB_BUCKET` are untouched - and they're valid against the branch, because it was bootstrapped with those exact values.

Also demonstrates: a **direct Kubernetes Secret** param source (`{ "secret": ..., "key": ... }`) - the token never needs to appear in the target pod's env at all.

</details>

<details>

<summary>Elasticsearch - bootstrap via <code>ELASTIC_PASSWORD</code> (ES 8 security)</summary>

Elasticsearch 8 ships with security enabled. The official image reads `ELASTIC_PASSWORD` as the `elastic` user's bootstrap password, so the branch is fed the app's real password through it. Dotted env names become `elasticsearch.yml` settings - here TLS is turned off so the app talks plain HTTP with basic auth:

```json
{
  "feature": {
    "db_branches": [
      {
        "type": "generic",
        "id": "my-elasticsearch-branch",
        "ttl_secs": 600,
        "creation_timeout_secs": 300,
        "image": "docker.elastic.co/elasticsearch/elasticsearch:8.17.4",
        "port": 9200,
        "connection": {
          "params": {
            "host": { "env_var_name": "ELASTICSEARCH_URL", "value_pattern": "https?://(?P<host>[^:/]+)" },
            "port": { "env_var_name": "ELASTICSEARCH_URL", "value_pattern": ":(?P<port>[0-9]+)$" },
            "password": "ELASTICSEARCH_PASSWORD"
          }
        },
        "env": {
          "discovery.type": "single-node",
          "ELASTIC_PASSWORD": "$(MIRRORD_PARAM_PASSWORD)",
          "xpack.security.http.ssl.enabled": "false",
          "ES_JAVA_OPTS": "-Xms512m -Xmx512m"
        },
        "readiness": {
          "type": "exec",
          "command": ["sh", "-c", "curl -s -u \"elastic:$ELASTIC_PASSWORD\" http://localhost:9200 > /dev/null"]
        }
      }
    ]
  }
}
```

Also demonstrates: an `exec` readiness probe - the command runs *inside* the branch container and can read the container's env with plain shell `$VAR` syntax, so Ready means "authentication actually works". JVM services need more memory than the branch pod default - see the resources note below the examples.

</details>

<details>

<summary>OpenSearch - HTTPS + security bootstrap</summary>

Same shape as Elasticsearch, using OpenSearch's `OPENSEARCH_INITIAL_ADMIN_PASSWORD` bootstrap. The demo security config keeps HTTPS on (self-signed certs), so the app connects with TLS verification disabled and the readiness probe curls with `-k`:

```json
{
  "feature": {
    "db_branches": [
      {
        "type": "generic",
        "id": "my-opensearch-branch",
        "ttl_secs": 600,
        "creation_timeout_secs": 300,
        "image": "opensearchproject/opensearch:2.19.1",
        "port": 9200,
        "connection": {
          "params": {
            "host": { "env_var_name": "OPENSEARCH_URL", "value_pattern": "https?://(?P<host>[^:/]+)" },
            "port": { "env_var_name": "OPENSEARCH_URL", "value_pattern": ":(?P<port>[0-9]+)$" },
            "password": "OPENSEARCH_PASSWORD"
          }
        },
        "env": {
          "discovery.type": "single-node",
          "OPENSEARCH_INITIAL_ADMIN_PASSWORD": "$(MIRRORD_PARAM_PASSWORD)",
          "OPENSEARCH_JAVA_OPTS": "-Xms512m -Xmx512m"
        },
        "readiness": {
          "type": "exec",
          "command": ["sh", "-c", "curl -sk -u \"admin:$OPENSEARCH_INITIAL_ADMIN_PASSWORD\" https://localhost:9200 > /dev/null"]
        }
      }
    ]
  }
}
```

Note the password must satisfy OpenSearch's initial-admin-password strength rules - which is fine, because it's the app's *real* password resolved from its Secret, not something mirrord invents.

</details>

<details>

<summary>Cassandra - JVM tuning via <code>env</code>, <code>exec</code> probe via cqlsh</summary>

Cassandra's default config has no authentication, so no credentials flow to the branch - the `env` here tunes the image itself (heap size), and readiness uses `cqlsh` inside the container. Note the probe command can be slow to start (`cqlsh` is a Python CLI); generic probes run with a 10-second timeout for exactly this reason:

```json
{
  "feature": {
    "db_branches": [
      {
        "type": "generic",
        "id": "my-cassandra-branch",
        "ttl_secs": 600,
        "creation_timeout_secs": 300,
        "image": "cassandra:5",
        "port": 9042,
        "connection": {
          "params": {
            "host": { "env_var_name": "CASSANDRA_ADDR", "value_pattern": "^(?P<host>[^:]+):" },
            "port": { "env_var_name": "CASSANDRA_ADDR", "value_pattern": ":(?P<port>[0-9]+)$" }
          }
        },
        "env": {
          "MAX_HEAP_SIZE": "512M",
          "HEAP_NEWSIZE": "128M"
        },
        "readiness": {
          "type": "exec",
          "command": ["sh", "-c", "cqlsh -e 'SELECT now() FROM system.local' > /dev/null 2>&1"]
        }
      }
    ]
  }
}
```

Cassandra needs roughly 2Gi even with a small heap (off-heap structures + JVM overhead) - see the resources note below.

</details>

<details>

<summary>Couchbase - <code>command</code>-wrapper bootstrap + host-only redirection (multi-port)</summary>

Couchbase is the hardest bootstrap class: the image has **no first-boot env mechanism** - a fresh node must be initialized with `couchbase-cli` after the server starts. The branch handles this with a `command` wrapper: start the server in the background, wait for the management API, then run `cluster-init` and `bucket-create` with the app's real password.

Two things to notice:

* The wrapper references the param with **plain shell syntax** (`$MIRRORD_PARAM_PASSWORD`) - the injected params are ordinary env vars on the branch container, so a shell script can read them directly; kubelet `$(...)` expansion is only needed when there is no shell in between.
* Couchbase is **multi-port** (management 8091, query 8093, KV 11210). mirrord redirects a single port - but if the app derives all its URLs from one *hostname* var, declaring only a `host` param (no `port`) redirects every port at once, since they all live on the same branch pod.

```json
{
  "feature": {
    "db_branches": [
      {
        "type": "generic",
        "id": "my-couchbase-branch",
        "ttl_secs": 600,
        "creation_timeout_secs": 300,
        "image": "couchbase:community-7.6.2",
        "port": 8091,
        "connection": {
          "params": {
            "host": { "env_var_name": "COUCHBASE_HOST", "value_pattern": "^(?P<host>.+)$" },
            "password": "COUCHBASE_PASSWORD"
          }
        },
        "command": [
          "/bin/bash", "-c",
          "/entrypoint.sh couchbase-server & until curl -s http://localhost:8091/pools > /dev/null; do sleep 2; done; couchbase-cli cluster-init -c localhost --cluster-username Administrator --cluster-password \"$MIRRORD_PARAM_PASSWORD\" --services data,index,query --cluster-ramsize 512 --cluster-index-ramsize 256; couchbase-cli bucket-create -c localhost -u Administrator -p \"$MIRRORD_PARAM_PASSWORD\" --bucket sandbox --bucket-type couchbase --bucket-ramsize 256; wait"
        ],
        "readiness": {
          "type": "exec",
          "command": ["sh", "-c", "curl -s -u \"Administrator:$MIRRORD_PARAM_PASSWORD\" http://localhost:8091/pools/default/buckets/sandbox > /dev/null"]
        }
      }
    ]
  }
}
```

The readiness probe checks that the `sandbox` bucket exists *with the bootstrapped credentials*, so the branch only turns Ready once the whole init sequence succeeded.

</details>

{% hint style="info" %}
**Resource-hungry images**: branch pods default to a 512Mi memory limit, which OOM-kills heavier services like Elasticsearch, OpenSearch, Cassandra, and Couchbase. Admins can raise it for all generic branches via `dbPod.resources` in the generic branch config (Helm `genericBranchConfig`), next to [`allowedImages`](#security). When a branch container does die, the branch fails immediately with the termination reason (e.g. `OOMKilled, exit code 137`) instead of hanging until the creation timeout.
{% endhint %}

## Readiness

The branch only turns Ready - and your session only proceeds - once the branch container's readiness probe passes. Because mirrord doesn't know your service, you can pick the probe that actually proves it's up:

| Type | Config | Ready when |
| --- | --- | --- |
| `tcp` *(default)* | `{ "type": "tcp" }` | The branch `port` accepts a TCP connection. No config needed - this is what you get when `readiness` is omitted. |
| `http_get` | `{ "type": "http_get", "path": "/health", "port": 8086 }` | An HTTP GET on `path` returns a 2xx/3xx. `port` defaults to the branch `port`. |
| `exec` | `{ "type": "exec", "command": ["valkey-cli", "ping"] }` | The command, run *inside* the branch container, exits 0. |

The probe runs every 2 seconds (first check after 3 seconds) with a 10-second timeout per attempt, so slow probe commands like `cqlsh` don't false-fail. The first success marks the branch Ready; if it never succeeds, the session fails once [`creation_timeout_secs`](../db-branching.md#key-fields) elapses.

Prefer a probe that captures "actually usable", not just "process started": a plain TCP probe can pass while an image is still initializing. The InfluxDB example uses `http_get` on `/health` precisely so the branch isn't Ready until the setup bootstrap has completed - with only the TCP default, the app could connect before its org and token exist.

## Copying Data into the Branch

By default a generic branch starts empty. When that isn't useful, add a `copy` config: after the empty branch boots and its readiness probe passes, the operator launches a one-shot Kubernetes Job from your copy image, and the branch stays **not Ready** until the Job succeeds. What "copy" means - full data, schema only, a filtered subset - is entirely up to your image; mirrord only wires the connections and gates readiness.

```json
{
  "type": "generic",
  "id": "my-valkey-branch",
  "image": "valkey/valkey:8-alpine",
  "port": 6379,
  "creation_timeout_secs": 300,
  "connection": { "params": { "host": ..., "port": ..., "password": "VALKEY_PASSWORD" } },
  "args": ["valkey-server", "--requirepass", "$(MIRRORD_PARAM_PASSWORD)"],
  "copy": {
    "image": "ghcr.io/my-org/valkey-copy:1.0",
    "command": ["./copy.sh"],
    "args": ["--mode=all"]
  }
}
```

| Field | Description |
| --- | --- |
| `copy.image` | *(Required)* Full image reference for the copy Job container. Goes through the same admin `allowedImages` policy as the branch image. |
| `copy.command` / `copy.args` | *(Optional)* Entrypoint override for the copy container. Values may reference the same `$(VAR)`s as the branch container, plus the `MIRRORD_BRANCH_*` built-ins below. |

### The copy Job's environment

The Job's env carries both ends of the copy - read from the source, write into the branch:

| Variable | Meaning |
| --- | --- |
| `MIRRORD_PARAM_<NAME>` | The same resolved connection params the branch container got (host/port/user/password/database plus your extras) - this is the **source** connection. Secret-backed params stay `secretKeyRef`s; the operator never reads their values. |
| `MIRRORD_BRANCH_HOST` | The branch pod's IP - where to write. |
| `MIRRORD_BRANCH_PORT` | The branch's resolved port. |
| `MIRRORD_BRANCH_ID` | The branch id. |
| `MIRRORD_DATABASE_NAME` | The shared `name` field, when set. |

Credentials for the branch side are the source params themselves: the empty branch was bootstrapped to accept them, so `MIRRORD_PARAM_PASSWORD` (or token, etc.) works against both ends.

### Semantics to know

* **The copy runs at most once per branch.** Unlike [migrations](migrations.md), it never re-runs when a Ready branch is reused by a new session, even if the new session's `copy` differs - re-running an arbitrary copy against populated data would duplicate it. Want a fresh copy? Use a new branch `id`.
* **Failure fails the branch.** A non-zero exit marks the branch `Failed`, and the Job's own logs land in the error your session prints - make your copy script fail loudly.
* **Both timeouts must cover the copy.** [`creation_timeout_secs`](../db-branching.md#key-fields) (how long the CLI waits for Ready, default 60s) and `ttl_secs` both keep counting while the copy runs. The operator refreshes the branch's TTL clock when the Job starts, so `ttl_secs` needs to cover the copy itself - and `creation_timeout_secs` needs to cover pod boot *plus* the copy.
* **NetworkPolicy**: the Job runs in the target's namespace and must reach both the source database and the branch pod's IP (the same constraint migration Jobs have).

## Admin Profiles: Branch Defaults and Copy

Writing a copy image and its invocation is often an admin's job, not every developer's. A named profile in the operator's Helm config can carry the whole engine setup - the branch container defaults *and* the copy Job:

```yaml
operator:
  genericBranchConfig:
    dbPod:
      allowedImages: ["opensearchproject/*", "registry.example.com/dbtools/*"]
    profiles:
      opensearch-full:
        dbPod:
          branch:
            image: "opensearchproject/opensearch:2.19.1"
            port: 9200
            env: { "discovery.type": "single-node" }
          copy:
            image: "registry.example.com/dbtools/opensearch-copy:1.0"
            command: ["./copy.sh", "--mode=all"]
```

A developer's mirrord.json then shrinks to the target-specific parts:

```json
{
  "type": "generic",
  "id": "my-opensearch-branch",
  "profile": "opensearch-full",
  "connection": { "params": { ... } }
}
```

Resolution is per field, and your mirrord.json always wins: a `copy` in the spec replaces the profile's entirely, an `image` in the spec overrides the profile's, and so on. `connection` always comes from mirrord.json - it describes *your target*, so a profile cannot supply it.

`dbPod.branch` and `dbPod.copy` are only honored inside a **named profile**. Setting them on the default-level `dbPod` fails generic branches with a clear error - the default applies to every generic branch regardless of engine, and users wanting an empty branch would have no opt-out.

## Connection

The `connection` must use **params mode**. URL mode is rejected for generic branches, because rewriting a whole URL requires knowing the engine's scheme and credential layout - for URL-shaped env vars, extract `host` and `port` with [`value_pattern`](connection.md#composite-environment-variables) as in the example above. All the other sources from [Connection Modes](connection.md) work (Kubernetes Secrets, literal values, multiple sources), except `gcp_secret_manager` and `aws_secrets_manager`, which are not supported for generic branches.

Declaring neither `host` nor `port` is allowed - the branch is still created and bootstrapped, but nothing is redirected (mirrord warns about this at config load).

## What Generic Branches Don't Do

* **No built-in data or schema copy** - branches start empty unless you (or an admin profile) supply a [copy Job](#copying-data-into-the-branch); the copy logic lives in your image, not in mirrord.
* **No IAM authentication.**
* **A single redirected port** - if the app derives all its endpoints from one *hostname* variable, declaring only a `host` param redirects every port to the branch pod (see the Couchbase example above); but cluster-shaped services that advertise their own addresses to clients are out of scope.
* **No SaaS-only services** - a generic branch runs a container image *in your cluster*, so the service must be self-hostable. Fully managed platforms with no container distribution (Databricks, Snowflake, and the like) have nothing to run. Two escape hatches: if a faithful local **emulator image** exists for the protocol, branch that instead - this is exactly how the built-in [DynamoDB engine](dynamodb.md) works (`amazon/dynamodb-local` in-cluster stands in for the AWS service); and if the managed service speaks a **standard protocol** - for example, Databricks Lakebase is Postgres-compatible - use the matching first-class engine (here, [PostgreSQL branching](postgresql.md)) or a stock image of that engine as the stand-in.

If you find yourself writing a generic config for a common engine, that's a strong signal for us to build first-class support for it - let us know!

## Security

* Generic branching is **off by default** (`operator.genericBranching` in the Helm chart) because it lets users who can create branches run arbitrary container images as branch pods. Enabling it is an explicit admin decision per cluster.
* Admins can restrict which images users may run with an `allowedImages` glob list in the generic branch config (Helm `genericBranchConfig`). When the field is absent, all images are allowed. The list covers the [copy Job's](#copying-data-into-the-branch) image too - both run user-chosen code in the cluster. A branch using an image outside the list fails immediately with an error naming the image:

```yaml
operator:
  genericBranching: true
  genericBranchConfig:
    dbPod:
      allowedImages:
        - "docker.io/library/*"
        - "ghcr.io/my-org/*"
```

* Generic branch pods run under the namespace **default service account**, not the target's and not the operator's. The service account token is **not mounted** into the branch pod at all - the container has zero Kubernetes API access even in clusters where roles were bound to the default SA. Branch pods are plain unprivileged pods, so cluster admission policies (PSA, OPA/Kyverno) apply as usual.
* Values written directly into `args` are visible in the pod spec to anyone with pod-read access. Reference secrets via `$(MIRRORD_PARAM_*)` instead of inlining them - the `secretKeyRef` mechanism exists precisely so secret values never appear in the pod spec.
* With operator `3.216.0` or later, images in private registries are pulled with the target's `imagePullSecrets`. For registries the target has no pull secret for, or on older operators, set `imagePullSecrets` in the generic branch config, as with the other engines.
