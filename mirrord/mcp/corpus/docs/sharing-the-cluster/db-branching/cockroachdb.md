---
title: CockroachDB
description: Spin up an isolated CockroachDB branch of your remote database with mirrord
tags:
  - alpha
  - team
  - enterprise
---

This page covers DB branching for CockroachDB. For the general concepts, the full list of config fields, and how a session behaves, see the [DB Branching overview](../db-branching.md).

{% hint style="info" %}
CockroachDB branching requires operator `3.186.0`, mirrord CLI `3.236.0`, and operator Helm chart `3.186.0` with the `operator.cockroachdbBranching` value set to `true`.
{% endhint %}

CockroachDB speaks the PostgreSQL wire protocol, so your application keeps its existing PostgreSQL driver and connection URL (the branch URL uses the `postgresql` scheme). The branch itself is a single-node `cockroachdb/cockroach` started with `start-single-node --insecure`, and the copy uses CockroachDB-native tooling (`cockroach sql`, `SHOW CREATE ALL TABLES`, `COPY ... TO/FROM STDOUT/STDIN WITH CSV`) rather than the PostgreSQL dump tools.

## Basic Configuration

```json
{
  "feature": {
    "db_branches": [
      {
        "id": "users-cockroachdb-db",
        "type": "cockroachdb",
        "version": "latest-v26.2",
        "name": "users-database-name",
        "connection": {
          "url": "DATABASE_URL"
        },
        "copy": {
          "mode": "empty"
        }
      }
    ]
  }
}
```

The `connection` field describes how mirrord locates the source database connection details - a full connection URL or individual parameters (host, port, user, password, database). CockroachDB uses port `26257` and user `root` when these are not specified. Because the branch runs in insecure mode, its connection URL carries `sslmode=disable`. See [Connection Modes](connection.md) for all supported sources, including Kubernetes Secrets, Google Secret Manager, literal values, and composite environment variables.

## Copy Modes

The `copy` field controls what data gets cloned when creating a CockroachDB branch.

| Mode | What gets cloned | Best for |
| --- | --- | --- |
| `"empty"` (default) | Nothing - an empty database with no schema or data | Workflows where your application initializes the schema or runs migrations as part of startup |
| `"schema"` | Only the table structures (schemas) from the source database, without any data | Testing schema changes or local development where structure is needed but data is not |
| `"all"` | Everything from the source database - both schema and data | A full clone of your environment data for debugging or reproducing production-like scenarios |

{% hint style="warning" %}
Use `"mode": "all"` with caution.
It's only recommended for very small or empty databases.
Copying large datasets can significantly increase branch creation time and storage usage.
{% endhint %}

### Filtered Data Clone

Developers can customize what gets copied per table. This allows copying only specific rows or subsets of data using SQL query filters.

```Json
{
  "copy": {
    "mode": "schema",                   // Or "empty" as explained below
    "tables": {
      "users": {
        "filter": "name = 'alice' OR name = 'bob'"
      },
      "orders": {
        "filter": "created_at > 1759948761"
      }
    }
  }
}
```

#### In this example

The schema for all tables is cloned.
The `users` table copy includes only rows for `alice` and `bob`.
The `orders` table copy includes only rows created after a certain timestamp.

Filtering can also be combined with `"mode": "empty"`, in which case only the specified tables (and their filtered data) are copied, while all others are excluded.

Note: Filtering is not compatible with `"mode": "all"`.
If both are specified, mirrord ignores the `tables` configuration.

{% hint style="info" %}
The `dump_args` field is not supported for CockroachDB. Only MySQL and PostgreSQL branches accept custom dump arguments.
{% endhint %}

## Source TLS and mutual TLS

With `"schema"` and `"all"` copy modes, the operator connects to your source database to copy from it. If the source's certificate is signed by a private CA (`sslmode=verify-ca`/`verify-full`), or the source requires mutual TLS (the client must present a certificate, as with CockroachDB's certificate authentication), the copy needs certificate files - otherwise it fails with `x509: certificate signed by unknown authority`.

Provide them in a `MirrordPropertyList` named `cockroachdb-source-tls` (the name can be changed cluster-wide with the operator Helm value `operator.cockroachdbBranchConfig.dbPod.sourceTlsPropertyList`), in the same namespace as the target workload. Keep certificate material in a Kubernetes `Secret` and reference it with `secretKeyRef` rather than inlining it:

```yaml
apiVersion: mirrord.metalbear.co/v1
kind: MirrordPropertyList
metadata:
  name: cockroachdb-source-tls
  namespace: my-app-namespace
spec:
  properties:
    - name: tlsCaCert
      valueFrom:
        secretKeyRef:
          name: my-db-certs
          key: ca.crt
    - name: tlsClientCert
      valueFrom:
        secretKeyRef:
          name: my-db-certs
          key: client.myservice.crt
    - name: tlsClientKey
      valueFrom:
        secretKeyRef:
          name: my-db-certs
          key: client.myservice.key
```

Supported properties:

| Property | Description | Required |
| --- | --- | --- |
| `tlsCaCert` | PEM CA bundle used to verify the source's certificate when it is not signed by a publicly trusted root. | No |
| `tlsClientCert` | PEM client certificate presented to a source that requires mutual TLS. Requires `tlsClientKey`. | No |
| `tlsClientKey` | PEM private key for `tlsClientCert`. Requires `tlsClientCert`. | No |

At least one property must be set. A source behind regular TLS with a private CA only needs `tlsCaCert`; sources requiring mutual TLS also need the client pair - the two always go together. These are the same TLS properties the [Temporal queue splitting connection](../queue-splitting/temporal.md) uses, and values resolve the same way: `secretKeyRef`, `configMapKeyRef`, and inline values all work.

The operator reads the properties when a branch is created, so rotated certificates are picked up by the next branch, not by ones already running.

The property list only provides the certificate files. Whether the copy connection uses TLS is decided by the `sslmode` in your source connection URL:

| `sslmode` in the URL | Property list exists? | Copy connection |
| --- | --- | --- |
| `verify-full` / `verify-ca` | yes | TLS, verified with your CA (and client certs, if set) |
| `verify-full` / `verify-ca` | no | Fails - there is no CA to verify against |
| `disable` | yes or no | Plain connection; the certificates are not used |
| not set | yes | Defaults to `verify-full` with your certificates |
| not set | no | Defaults to `disable` |

In short: the URL decides *whether* to use TLS, the property list decides *with which certificates*.

In params mode, set the mode with the `sslmode` connection param, either as a [literal value](connection.md#literal-value) or from an env var on the target pod (all [value sources](connection.md) work):

```json
{
  "connection": {
    "type": "env",
    "params": {
      "host": "DB_HOST",
      "user": "DB_USER",
      "password": "DB_PASSWORD",
      "database": "DB_NAME",
      "sslmode": { "env_var_name": "DB_SSLMODE", "value": "verify-full" }
    }
  }
}
```

To read the mode from the target pod's environment instead, use a plain string: `"sslmode": "DB_SSLMODE"`.

Cluster admins can set a default mode for all branches with the operator Helm value `operator.cockroachdbBranchConfig.dbPod.sourceSslmode`; an explicit `sslmode` in a session's URL or params still wins over it.

The certificates are only used for the copy connection to the source. The branch itself runs in insecure mode and the connection URL handed to your application carries `sslmode=disable`, so neither the branch nor your locally running process needs any certificates. `"empty"` mode never contacts the source and works without this setup entirely.
