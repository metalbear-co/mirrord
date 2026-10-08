---
title: Redis
description: Spin up an isolated Redis branch with mirrord - in the cluster or on your own machine
tags:
  - alpha
  - team
  - enterprise
---

This page covers DB branching for Redis. For the general concepts, the full list of config fields, and how a session behaves, see the [DB Branching overview](../db-branching.md).

Redis is the only engine that supports both branch locations:
* `"location": "remote"` (default) provisions a branch pod in the cluster, like the other engines.
* `"location": "local"` spawns a Redis instance on your own machine.

{% hint style="info" %}
Remote Redis branching requires operator `3.168.0`, mirrord CLI `3.217.0`, and operator Helm chart `3.168.0` with the `operator.redisBranching` value set to `true`.
Local Redis branching only requires mirrord CLI `3.180.0` - there are no operator or chart version requirements, since a local branch runs entirely on your machine.
{% endhint %}

## Remote Branches

```json
{
  "feature": {
    "db_branches": [
      {
        "id": "cache-redis",
        "type": "redis",
        "version": "7.2",
        "name": "0",
        "connection": {
          "url": "REDIS_URL"
        },
        "copy": {
          "mode": "empty"
        }
      }
    ]
  }
}
```

The `connection` field describes how mirrord locates the source Redis connection details - a full connection URL or individual parameters (host, port, user, password, database). See [Connection Modes](connection.md) for all supported sources, including Kubernetes Secrets, Google Secret Manager, literal values, and composite environment variables.

### Database Index

For Redis, the `name` field is the database **index** Redis uses to select a logical database rather than a name, so it must be a valid non-negative number. If omitted, it defaults to index `0`.

## Copy Modes

Redis copy modes apply only to **remote** Redis branches. A local Redis branch always starts empty. Redis supports two modes:

| Mode | What gets cloned |
| --- | --- |
| `"empty"` (default) | Nothing - a fresh, empty Redis instance |
| `"all"` | Keys from the source Redis instance, optionally filtered with `patterns` |

By default `"all"` copies the entire keyspace. To copy only a subset, add `patterns` - a list of [`SCAN MATCH`](https://redis.io/docs/latest/commands/scan/) glob patterns. Only keys matching at least one pattern are copied.

```json
{
  "copy": {
    "mode": "all",
    "patterns": ["user:*", "session:*"]
  }
}
```

In this example, only keys prefixed with `user:` or `session:` are copied; everything else is left out of the branch.

{% hint style="info" %}
Redis has no schema, so there is no `"schema"` copy mode - only `"empty"` and `"all"` are available. Filtering is done by key pattern rather than by SQL query.
{% endhint %}

## TLS-Enabled Custom Images

By default the operator starts a remote branch pod with its own `redis-server` command, so TLS configuration baked into a custom image is not picked up and the branch serves plain TCP. If your apps connect to Redis over TLS (or your platform requires it), configure the branch pod via the `operator.redisBranchConfig` value in the [mirrord-operator Helm chart](https://github.com/metalbear-co/charts/blob/main/mirrord-operator/values.yaml):

```yaml
redisBranchConfig:
  dbPod:
    image:
      registry: "myregistry/redis-tls"   # custom image with certificates baked in
    dbServerArgs:                        # appended to the operator's redis-server command
      - "--tls-port"
      - "6379"
      - "--port"
      - "0"
      - "--tls-cert-file"
      - "/certs/tls.crt"
      - "--tls-key-file"
      - "/certs/tls.key"
      - "--tls-auth-clients"
      - "no"
    tls: true                            # the operator connects to the branch over TLS
```

The two settings play different roles:

* `dbServerArgs` - extra flags appended to the branch's `redis-server` command, after the operator-managed `--requirepass`. Use them to enable the TLS listener with the certificate paths that exist inside your image.
* `tls` - tells the **operator** that the branch only accepts TLS: the data-copy step connects over TLS, and the connection URL injected into your app uses the `rediss://` scheme.

Requirements:

* Keep the TLS listener on the default port (`--tls-port 6379`) and disable the plaintext listener (`--port 0`). The operator always points your app and its own data-copy step at port `6379` on the branch pod.
* The certificate paths passed in `dbServerArgs` must exist inside the image - build them into your custom Redis image, since the operator does not mount certificate volumes into branch pods.
* The config applies when a branch is **created**; branches that already exist keep the mode they started with.
* `redisBranchConfig.dbPod` is one cluster-wide setting, so every Redis branch gets the same TLS mode. To run TLS and plain Redis branches in the same cluster, define each as a profile and let branches pick one - see [Branch Config Profiles](../db-branching.md#branch-config-profiles).

{% hint style="info" %}
TLS branch pods require operator and Helm chart `3.183.0` or later. Local Redis branches are unaffected - they always run a plain local instance.
{% endhint %}

## Copying From a TLS Source

With `copy.mode: "all"`, the branch pod's copy step connects to the **source** Redis to read its keys. When the source only accepts TLS, provide the certificate material in a `MirrordPropertyList` named `redis-source-tls` (the name can be changed cluster-wide with the operator Helm value `operator.redisBranchConfig.dbPod.sourceTlsPropertyList`), in the same namespace as the target workload. Keep certificate material in a Kubernetes `Secret` and reference it with `secretKeyRef` rather than inlining it:

```yaml
apiVersion: mirrord.metalbear.co/v1
kind: MirrordPropertyList
metadata:
  name: redis-source-tls
  namespace: my-app-namespace
spec:
  properties:
    - name: tlsCaCert
      valueFrom:
        secretKeyRef:
          name: my-redis-certs
          key: ca.crt
```

Supported properties:

| Property | Description | Required |
| --- | --- | --- |
| `tlsCaCert` | PEM CA bundle used to verify the source's certificate when it is not signed by a publicly trusted root. | No |
| `tlsClientCert` | PEM client certificate presented to a source that requires mutual TLS (`--tls-auth-clients yes`). Requires `tlsClientKey`. | No |
| `tlsClientKey` | PEM private key for `tlsClientCert`. Requires `tlsClientCert`. | No |

At least one property must be set. A source behind regular TLS with a private CA only needs `tlsCaCert`; a source signed by a publicly trusted root can carry that public root in `tlsCaCert` too. These are the same TLS properties the [CockroachDB source TLS](cockroachdb.md) setup uses, and values resolve the same way: `secretKeyRef`, `configMapKeyRef`, and inline values all work.

The property list's presence makes the copy connection use TLS: a plain `redis://` source URL is upgraded to `rediss://`, and connection-parameter sources (`host`/`port` env vars) connect over TLS as well. Without the property list, the URL's own scheme decides - a `rediss://` source URL connects with TLS against the system's trusted roots, and appending `#insecure` to it skips certificate verification entirely.

The operator reads the properties when a branch is created, so rotated certificates are picked up by the next branch, not by ones already running.

{% hint style="info" %}
Copying from a TLS source requires operator and Helm chart `3.199.0` or later.
{% endhint %}

## Local Redis

mirrord can spin up a local Redis instance, automatically redirecting your app's Redis traffic to it.

```json
{
  "feature": {
    "db_branches": [
      {
        "type": "redis",
        "location": "local",                     // "remote" (default) or "local"
        "connection": {
          // Use "host" if your app reads Redis as host:port (e.g. REDIS_ADDR=redis:6379)
          // Use "url" if your app reads a full Redis URL (e.g. REDIS_URL=redis://user:pass@redis:6379/0)
          "host": { "type": "env", "variable": "REDIS_ADDR" }
        },
        "local": {                               // Optional runtime config
          "port": 6379,                          // Custom port (default: 6379)
          "runtime": "container",                // "container" (default), "redis_server", or "auto"
          "container_runtime": "docker"          // "docker" (default), "podman", or "nerdctl"
        }
      }
    ]
  }
}
```

mirrord overrides the env variable to point to `localhost:<port>` and cleans up the Redis instance on exit.

Notes on local branches:
* A local branch always starts empty - copy modes do not apply.
* The `id` field is not used for local Redis instances and has no effect on database selection or reuse.
* `local.port`: Sessions that use the same port share a single local Redis database. When a new session starts on that port, it creates a new database instance that replaces the existing one.
