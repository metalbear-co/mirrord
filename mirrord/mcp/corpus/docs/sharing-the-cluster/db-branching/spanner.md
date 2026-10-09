---
title: Google Spanner
description: Spin up an isolated Google Cloud Spanner branch of your remote database with mirrord
tags:
  - alpha
  - team
  - enterprise
---

This page covers DB branching for Google Cloud Spanner. For the general concepts, the full list of config fields, and how a session behaves, see the [DB Branching overview](../db-branching.md).

The branch runs the [Cloud Spanner emulator](https://cloud.google.com/spanner/docs/emulator) as a pod in your cluster. mirrord redirects your app to it by setting `SPANNER_EMULATOR_HOST`, which every official Spanner client library checks on its own. Your app keeps its own `project`, `instance`, and `database` values unchanged - they now resolve against the emulator instead of real Spanner, so no code change is needed.

{% hint style="info" %}
Spanner branching requires operator `3.182.0`, mirrord CLI `3.230.0`, and operator Helm chart `3.182.0` with the `operator.spannerBranching` value set to `true`.
{% endhint %}

## Basic Configuration

```json
{
  "feature": {
    "db_branches": [
      {
        "id": "users-spanner-db",
        "type": "spanner",
        "version": "1.5.23",
        "connection": {
          "params": {
            "project": "SPANNER_PROJECT_ID",
            "instance": "SPANNER_INSTANCE_ID",
            "database_id": "SPANNER_DATABASE_ID"
          }
        },
        "copy": {
          "mode": "schema"
        }
      }
    ]
  }
}
```

`version` is the Cloud Spanner emulator image tag to run for the branch.

## Connection

Spanner locates the source database through individual parameters under `connection.params`, each naming an environment variable on the target pod that holds the value:

| Param | Description |
| --- | --- |
| `project` | Env var holding the source Google Cloud project ID |
| `instance` | Env var holding the source Spanner instance ID |
| `database_id` | Env var holding the source Spanner database ID |

These identifiers are read-only: the operator resolves them from the target pod so the branch can recreate the same instance and database in the emulator, and the app is never overridden - it is redirected wholesale by `SPANNER_EMULATOR_HOST`. This is why the key is `database_id` rather than `database`: the fixed `database` slot is an override target, which does not apply to Spanner. See [Connection Modes](connection.md) for all supported sources, including Kubernetes Secrets and Google Secret Manager.

## Authentication

The `empty` mode copies nothing, so it needs no credentials. For `schema` and `all`, the branch reads the source Spanner over the Google API and authenticates as the **target pod's own Google identity** through Application Default Credentials - the same credential detection used for GCP Cloud SQL. There is no `iam_auth` block, and nothing GCP-related is configured on the operator: it inherits whatever identity your workload already uses. Make sure that identity has read access (for example `roles/spanner.databaseReader`) to the source database.

mirrord picks up the identity from the target pod in one of two ways:

- **Workload Identity (GKE):** the pod runs as a Kubernetes service account bound to a Google service account. The branch runs under the same service account, so ADC resolves it with no key file.
- **Mounted key file:** the pod sets `GOOGLE_APPLICATION_CREDENTIALS` to a mounted service-account key. The operator mounts that same key on the branch and points ADC at it.

## Copy Modes

The `copy` field controls what gets cloned when creating a Spanner branch.

| Mode | What gets cloned | Best for |
| --- | --- | --- |
| `"empty"` (default) | Nothing - the instance and database are created in the emulator, but no schema or data | Workflows where your application creates its own schema or runs migrations as part of startup |
| `"schema"` | Only the table structures (DDL) of the source database, without any data | Testing schema changes or local development where structure is needed but data is not |
| `"all"` | Everything from the source database - both schema and data | A full clone of your environment data for debugging or reproducing production-like scenarios |

{% hint style="warning" %}
Use `"mode": "all"` with caution.
It's only recommended for very small or empty databases.
Copying large datasets can significantly increase branch creation time and storage usage.
{% endhint %}

### Filtered Data Clone

You can customize what gets copied per table using SQL query filters, so the branch carries only the rows you need. Each `filter` is a Spanner SQL `WHERE` clause applied when reading the source table.

```json
{
  "copy": {
    "mode": "schema",
    "tables": {
      "Users": {
        "filter": "Id = 1 OR Id = 2"
      },
      "Orders": {
        "filter": "Total > 100"
      }
    }
  }
}
```

#### In this example

The schema for all tables is cloned.
The `Users` table copy includes only rows with `Id` 1 or 2.
The `Orders` table copy includes only rows with `Total` greater than 100.

Filtering can also be combined with `"mode": "empty"`, in which case only the specified tables (and their filtered data) are copied, while all others are excluded.

Note: Filtering is not compatible with `"mode": "all"`.
If both are specified, mirrord ignores the `tables` configuration.

## Redirecting a Custom Emulator Variable

By default mirrord writes the branch emulator's address into `SPANNER_EMULATOR_HOST`, which the official client libraries detect automatically. If your app reads the emulator endpoint from a differently-named variable, set `emulator_host` to that variable's name so mirrord writes the address where your app looks:

```json
{
  "feature": {
    "db_branches": [
      {
        "type": "spanner",
        "version": "1.5.23",
        "emulator_host": "MY_SPANNER_EMULATOR",
        "connection": {
          "params": {
            "project": "SPANNER_PROJECT_ID",
            "instance": "SPANNER_INSTANCE_ID",
            "database_id": "SPANNER_DATABASE_ID"
          }
        },
        "copy": { "mode": "schema" }
      }
    ]
  }
}
```

`emulator_host` is the name of the variable mirrord sets, not an address - mirrord fills its value with the branch emulator's `host:port` at runtime.
